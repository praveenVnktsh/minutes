// audio/mic_check.rs
//
// The setup check that closes onboarding: prove the downloaded model loads,
// then record a few seconds of the microphone and of the system audio at the
// same time, show the user a meter per channel, and turn each capture into text
// on its own.
//
// The three things it checks are reported separately and never rolled into a
// single verdict. A Mac with no meeting running is supposed to finish with a
// working microphone and a silent system-audio channel, and the user has to
// read that as one good result and one neutral one; a composite pass/fail would
// turn the expected outcome into a failure. For the same reason every way a
// channel can go wrong gets its own outcome, because the frontend has a
// specific fix to offer for each one and a generic failure teaches the user
// nothing.
//
// Capturing a channel — the cpal thread, the meter, the platform paths for
// system audio — belongs to `capture_probe`. This module owns the order the
// check runs in, the classification of what came back, and the wire shapes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};

use super::audio_processing::{audio_to_mono, resample_audio};
use super::capture_probe::{peak_of, probe_channel, ProbeCapture, ProbeChannel, ProbeError};
use crate::parakeet_engine::commands::{parakeet_init, PARAKEET_ENGINE};
use crate::parakeet_engine::{ModelStatus, ParakeetEngine};

const DEFAULT_DURATION_SECS: f64 = 8.0;
const MIN_DURATION_SECS: f64 = 3.0;
const MAX_DURATION_SECS: f64 = 15.0;

/// A peak under this is indistinguishable from a muted device or a channel
/// nobody is putting sound into — roughly -40 dBFS.
const SILENCE_FLOOR: f32 = 0.01;

/// Parakeet wants mono f32 at this rate.
const TRANSCRIBE_SAMPLE_RATE: u32 = 16_000;
/// The model onboarding downloads, and so the one most likely to be on disk.
const PREFERRED_MODEL: &str = "parakeet-tdt-0.6b-v3-int8";

/// Only one check may own the audio devices at a time.
static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Raised by `mic_check_cancel`, cleared when a check takes the flag above.
///
/// It lives behind an `Arc` because both probes of a check hold a clone of it:
/// stopping the check has to reach two captures running concurrently, not one.
static CANCEL_REQUESTED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn cancel_flag() -> Arc<AtomicBool> {
    CANCEL_REQUESTED
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// How the model step ended. It is a first-class step of its own, not a
/// footnote on a channel: the user can retry it or re-download the weights, and
/// those fixes have nothing to do with a microphone.
///
/// The TypeScript union carries one more variant, `command_failed`, which is
/// synthesized in the frontend when the invoke itself throws — precisely the
/// case Rust cannot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCheckOutcome {
    /// The weights are loaded and the engine answered.
    Loaded,
    /// There is no model on disk to load.
    Unavailable,
    /// Initialising, discovering or loading a model errored.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReport {
    pub outcome: ModelCheckOutcome,
    /// The model that was loaded, when one was.
    pub model_name: Option<String>,
    /// The underlying error, for the log and a details line. Never the whole
    /// message shown to the user — the outcome decides that.
    pub detail: Option<String>,
}

impl ModelReport {
    fn loaded(model_name: Option<String>) -> Self {
        Self {
            outcome: ModelCheckOutcome::Loaded,
            model_name,
            detail: None,
        }
    }

    fn failed(outcome: ModelCheckOutcome, detail: Option<String>) -> Self {
        Self {
            outcome,
            model_name: None,
            detail,
        }
    }
}

/// How one channel of the check ended. Every variant is a different sentence
/// with a different fix in the frontend, which switches on all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelOutcome {
    /// Words came back.
    Transcribed,
    /// Audible signal, but the model returned nothing.
    NoSpeechDetected,
    /// The channel opened fine and sent silence.
    NoAudioDetected,
    /// The OS withheld the audio.
    PermissionDenied,
    /// The device could not be resolved or opened.
    DeviceUnavailable,
    /// The model errored on this channel's buffer.
    TranscriptionFailed,
    /// The user stopped the check.
    Cancelled,
    /// This platform cannot capture this channel here.
    Unsupported,
    /// The model never loaded, so nothing was captured on this channel.
    NotRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelReport {
    /// Reused from the probe rather than redeclared, so the `channel` in a
    /// report and the `channel` in a level event can never drift apart.
    pub channel: ProbeChannel,
    pub outcome: ChannelOutcome,
    /// Only set when this channel actually produced words.
    pub transcript: Option<String>,
    /// The device this channel opened, or the name that was asked for when it
    /// never opened. Left empty rather than filled with an invented label: the
    /// frontend already has per-channel wording for a device it cannot name.
    pub device_name: String,
    pub peak_level: f32,
    pub duration_ms: u64,
    pub detail: Option<String>,
}

impl ChannelReport {
    fn new(channel: ProbeChannel, outcome: ChannelOutcome, device_name: String) -> Self {
        Self {
            channel,
            outcome,
            transcript: None,
            device_name,
            peak_level: 0.0,
            duration_ms: 0,
            detail: None,
        }
    }

    fn with_capture(mut self, peak_level: f32, duration_ms: u64) -> Self {
        self.peak_level = peak_level;
        self.duration_ms = duration_ms;
        self
    }

    fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCheckResult {
    pub model: ModelReport,
    pub microphone: ChannelReport,
    pub system_audio: ChannelReport,
    /// How long the capture window actually ran, which is shorter than the
    /// requested duration when the user stopped it.
    pub duration_ms: u64,
    pub cancelled: bool,
}

/// Clears `CHECK_IN_FLIGHT` however the command exits, and owns the cancel flag
/// the probes share for the length of one check.
struct InFlightGuard {
    cancelled: Arc<AtomicBool>,
}

impl InFlightGuard {
    fn acquire() -> Option<Self> {
        CHECK_IN_FLIGHT
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| {
                let cancelled = cancel_flag();
                // A cancel that arrived between two checks must not kill this one.
                cancelled.store(false, Ordering::SeqCst);
                Self { cancelled }
            })
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.cancelled.store(false, Ordering::SeqCst);
        CHECK_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// Load the model, record the microphone and the system audio over one window,
/// and report each of the three on its own.
#[tauri::command]
pub async fn mic_check_start<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    duration_secs: Option<f64>,
) -> Result<SetupCheckResult, String> {
    let guard = match InFlightGuard::acquire() {
        Some(guard) => guard,
        None => return Err("A setup check is already running".to_string()),
    };
    let cancelled = guard.cancelled.clone();

    let duration = capture_duration(duration_secs);

    // The model comes first, and once. Loading it is the first time in the
    // app's life these weights are exercised, so the log line below is what a
    // support thread will ask for.
    let (engine, model) = match ready_engine().await {
        Ok((engine, model_name)) => {
            info!(
                "Setup check loaded the transcription model: {}",
                model_name.as_deref().unwrap_or("unnamed model")
            );
            (engine, ModelReport::loaded(model_name))
        }
        Err((outcome, detail)) => {
            warn!(
                "Setup check has no usable Parakeet model: {:?} ({:?})",
                outcome, detail
            );
            // Nothing is captured when the model did not load. A microphone
            // verdict the user cannot act on, stacked under a model failure
            // they can, is noise at the moment they most need one clear
            // instruction.
            return Ok(nothing_captured(
                ModelReport::failed(outcome, detail),
                mic_device_name.as_deref(),
                system_device_name.as_deref(),
            ));
        }
    };

    info!(
        "Setup check capturing the microphone and system audio for {} ms",
        duration.as_millis()
    );

    // Both channels share one window so the user waits once and watches two
    // meters, rather than sitting through two consecutive eight-second stares.
    let started = Instant::now();
    let (microphone_probe, system_probe) = tokio::join!(
        probe_channel(
            app.clone(),
            ProbeChannel::Microphone,
            mic_device_name.clone(),
            duration,
            cancelled.clone(),
        ),
        probe_channel(
            app,
            ProbeChannel::SystemAudio,
            system_device_name.clone(),
            duration,
            cancelled.clone(),
        ),
    );
    let window_ms = started.elapsed().as_millis() as u64;

    // The two transcriptions run one after another rather than joined: there is
    // a single engine behind them, and letting two few-second buffers contend
    // for it buys nothing worth the contention.
    let microphone = report_channel(
        &engine,
        ProbeChannel::Microphone,
        mic_device_name.as_deref(),
        microphone_probe,
        &cancelled,
    )
    .await;
    let system_audio = report_channel(
        &engine,
        ProbeChannel::SystemAudio,
        system_device_name.as_deref(),
        system_probe,
        &cancelled,
    )
    .await;

    let result = SetupCheckResult {
        model,
        microphone,
        system_audio,
        duration_ms: window_ms,
        cancelled: cancelled.load(Ordering::SeqCst),
    };
    info!(
        "Setup check finished in {} ms: model {:?}, microphone {:?}, system audio {:?}",
        result.duration_ms,
        result.model.outcome,
        result.microphone.outcome,
        result.system_audio.outcome
    );
    Ok(result)
}

/// Stop a check that is still running; the pending `mic_check_start` then
/// resolves with both channels cancelled.
#[tauri::command]
pub async fn mic_check_cancel() -> Result<(), String> {
    if CHECK_IN_FLIGHT.load(Ordering::SeqCst) {
        info!("Setup check cancellation requested");
        cancel_flag().store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// The duration to record, defaulted and clamped to something a user will sit
/// through but that still gives the model a sentence to work with.
fn capture_duration(duration_secs: Option<f64>) -> Duration {
    let seconds = match duration_secs {
        Some(seconds) if seconds.is_finite() => seconds.clamp(MIN_DURATION_SECS, MAX_DURATION_SECS),
        _ => DEFAULT_DURATION_SECS,
    };
    Duration::from_secs_f64(seconds)
}

/// The result when the model never loaded: both channels say they were never
/// run, and the model report carries the one thing the user can act on.
fn nothing_captured(
    model: ModelReport,
    mic_device_name: Option<&str>,
    system_device_name: Option<&str>,
) -> SetupCheckResult {
    SetupCheckResult {
        model,
        microphone: ChannelReport::new(
            ProbeChannel::Microphone,
            ChannelOutcome::NotRun,
            requested_name(mic_device_name),
        ),
        system_audio: ChannelReport::new(
            ProbeChannel::SystemAudio,
            ChannelOutcome::NotRun,
            requested_name(system_device_name),
        ),
        duration_ms: 0,
        cancelled: false,
    }
}

/// The name the caller asked for, for a channel that never opened a device. An
/// empty string is deliberate: the frontend words an unnamed channel itself.
fn requested_name(device_name: Option<&str>) -> String {
    device_name.unwrap_or_default().to_string()
}

/// Everything one channel is guilty of, decided from its own samples and its
/// own transcription. Neither channel's verdict depends on the other's.
async fn report_channel(
    engine: &ParakeetEngine,
    channel: ProbeChannel,
    device_name: Option<&str>,
    probe: Result<ProbeCapture, ProbeError>,
    cancelled: &AtomicBool,
) -> ChannelReport {
    let capture = match probe {
        Ok(capture) => capture,
        Err(error) => {
            // A capture the user stopped while the device was still opening is
            // their doing, not the device's; calling it unavailable would send
            // them chasing a fault they created.
            if cancelled.load(Ordering::SeqCst) {
                return ChannelReport::new(
                    channel,
                    ChannelOutcome::Cancelled,
                    requested_name(device_name),
                );
            }

            let outcome = match error {
                ProbeError::Unsupported(_) => ChannelOutcome::Unsupported,
                ProbeError::DeviceUnavailable(_) => ChannelOutcome::DeviceUnavailable,
            };
            warn!("Setup check could not capture {}: {}", channel, error);
            return ChannelReport::new(channel, outcome, requested_name(device_name))
                .with_detail(Some(error.to_string()));
        }
    };

    let peak = peak_of(&capture.samples);
    let duration_ms = capture.duration_ms();
    let report = |outcome| {
        ChannelReport::new(channel, outcome, capture.device_name.clone())
            .with_capture(peak, duration_ms)
    };

    if capture.cancelled || cancelled.load(Ordering::SeqCst) {
        info!(
            "Setup check cancelled after {} ms of {} audio",
            duration_ms, channel
        );
        return report(ChannelOutcome::Cancelled);
    }

    // Silence is diagnosed from the samples alone; there is nothing in it for
    // the model to work with and its verdict would only muddy the message.
    if let Some(outcome) = classify_capture(channel, &capture.samples, peak) {
        info!(
            "Setup check captured no usable {} audio from '{}': {:?} (peak {:.4}, {} ms)",
            channel, capture.device_name, outcome, peak, duration_ms
        );
        return report(outcome);
    }

    let mono = audio_to_mono(&capture.samples, capture.channels);
    let resampled = resample_audio(&mono, capture.sample_rate, TRANSCRIBE_SAMPLE_RATE);

    let transcript = match engine.transcribe_audio(resampled).await {
        Ok(transcript) => transcript,
        Err(error) => {
            error!("Setup check {} transcription failed: {}", channel, error);
            return report(ChannelOutcome::TranscriptionFailed)
                .with_detail(Some(error.to_string()));
        }
    };

    let transcript = transcript.trim().to_string();
    if transcript.is_empty() {
        info!(
            "Setup check heard {} audio but no speech (peak {:.4}, {} ms)",
            channel, peak, duration_ms
        );
        return report(ChannelOutcome::NoSpeechDetected);
    }

    info!(
        "Setup check transcribed {} audio from '{}' ({} ms, peak {:.4})",
        channel, capture.device_name, duration_ms, peak
    );
    let mut transcribed = report(ChannelOutcome::Transcribed);
    transcribed.transcript = Some(transcript);
    transcribed
}

/// The verdict this channel's captured audio alone supports. `None` means it is
/// worth sending to the model, which then decides between speech and silence.
fn classify_capture(channel: ProbeChannel, samples: &[f32], peak: f32) -> Option<ChannelOutcome> {
    // A channel that handed back not one callback and a channel that handed
    // back bit-exact zeroes look alike on screen but are different faults, so
    // they are diagnosed apart.
    if samples.is_empty() {
        return Some(no_capture_outcome(channel));
    }
    if samples.iter().all(|sample| *sample == 0.0) {
        return Some(digital_silence_outcome(channel));
    }
    if peak < SILENCE_FLOOR {
        return Some(ChannelOutcome::NoAudioDetected);
    }
    None
}

/// What a channel that produced no samples at all is guilty of.
///
/// A microphone stream that opened, ran for the whole window and delivered not
/// one callback is the operating system withholding the audio, on every
/// platform — a quiet room still produces samples. System audio is the
/// exception, for the reason given on [`digital_silence_outcome`].
fn no_capture_outcome(channel: ProbeChannel) -> ChannelOutcome {
    if matches!(channel, ProbeChannel::SystemAudio) {
        return ChannelOutcome::NoAudioDetected;
    }

    ChannelOutcome::PermissionDenied
}

/// What a channel that produced nothing but bit-exact zeroes is guilty of.
///
/// macOS hands a denied app a stream of digital silence instead of refusing to
/// open the microphone, so zeroes there indict the permission rather than the
/// device. Everywhere else they are simply a muted or wrong input.
///
/// System audio is the exception on every platform: a Mac with nothing playing
/// through its speakers produces exactly this, and it is the expected clean
/// outcome of the check rather than a fault. Calling it a permission problem
/// would send a user whose setup is fine into System Settings. The frontend's
/// wording for `no_audio_detected` on this channel names both possibilities —
/// nothing was playing, or the tap is not permitted.
fn digital_silence_outcome(channel: ProbeChannel) -> ChannelOutcome {
    if matches!(channel, ProbeChannel::SystemAudio) {
        return ChannelOutcome::NoAudioDetected;
    }

    #[cfg(target_os = "macos")]
    {
        ChannelOutcome::PermissionDenied
    }
    #[cfg(not(target_os = "macos"))]
    {
        ChannelOutcome::NoAudioDetected
    }
}

fn parakeet_engine() -> Option<Arc<ParakeetEngine>> {
    PARAKEET_ENGINE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .cloned()
}

/// Bring the shared Parakeet engine up with a model loaded, or say which kind
/// of model problem the user has. The name that comes back is the model that is
/// now loaded, which is what the frontend shows.
async fn ready_engine(
) -> Result<(Arc<ParakeetEngine>, Option<String>), (ModelCheckOutcome, Option<String>)> {
    if parakeet_engine().is_none() {
        parakeet_init()
            .await
            .map_err(|error| (ModelCheckOutcome::Failed, Some(error)))?;
    }

    let engine = parakeet_engine().ok_or((
        ModelCheckOutcome::Failed,
        Some("Parakeet engine not initialized".to_string()),
    ))?;

    // A model the app already loaded is the one the user will record with, so
    // the check exercises that rather than reloading weights for its own sake.
    if engine.is_model_loaded().await {
        let name = engine.get_current_model().await;
        return Ok((engine, name));
    }

    let models = engine
        .discover_models()
        .await
        .map_err(|error| (ModelCheckOutcome::Failed, Some(error.to_string())))?;
    let available: Vec<_> = models
        .iter()
        .filter(|model| matches!(model.status, ModelStatus::Available))
        .collect();

    let model = available
        .iter()
        .find(|model| model.name == PREFERRED_MODEL)
        .or_else(|| available.first())
        .ok_or((ModelCheckOutcome::Unavailable, None))?;

    info!("Setup check loading Parakeet model '{}'", model.name);
    engine
        .load_model(&model.name)
        .await
        .map_err(|error| (ModelCheckOutcome::Failed, Some(error.to_string())))?;

    Ok((engine, Some(model.name.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_report(channel: ProbeChannel, outcome: ChannelOutcome) -> ChannelReport {
        ChannelReport::new(channel, outcome, "MacBook Pro Microphone".to_string())
            .with_capture(0.5, 8000)
    }

    #[test]
    fn duration_defaults_to_eight_seconds() {
        assert_eq!(capture_duration(None), Duration::from_secs(8));
        assert_eq!(capture_duration(Some(f64::NAN)), Duration::from_secs(8));
    }

    #[test]
    fn duration_is_clamped_to_a_range_a_user_will_sit_through() {
        assert_eq!(capture_duration(Some(0.5)), Duration::from_secs(3));
        assert_eq!(capture_duration(Some(600.0)), Duration::from_secs(15));
        assert_eq!(capture_duration(Some(-4.0)), Duration::from_secs(3));
        assert_eq!(capture_duration(Some(5.0)), Duration::from_secs(5));
    }

    #[test]
    fn a_microphone_that_handed_back_nothing_means_the_os_withheld_the_audio() {
        // On every platform, not just macOS: a quiet room still produces
        // samples, so an empty buffer is the OS holding the audio back rather
        // than a microphone with nothing to hear.
        let outcome = classify_capture(ProbeChannel::Microphone, &[], 0.0);
        assert_eq!(outcome, Some(ChannelOutcome::PermissionDenied));
    }

    #[test]
    fn microphone_digital_silence_indicts_the_permission_on_macos_only() {
        let outcome = classify_capture(ProbeChannel::Microphone, &[0.0; 4096], 0.0);

        #[cfg(target_os = "macos")]
        assert_eq!(outcome, Some(ChannelOutcome::PermissionDenied));

        #[cfg(not(target_os = "macos"))]
        assert_eq!(outcome, Some(ChannelOutcome::NoAudioDetected));
    }

    #[test]
    fn system_audio_silence_is_never_a_permission_problem_on_any_platform() {
        // Nothing playing through the speakers is the expected clean outcome of
        // the check, not a fault, and it looks the same on every platform.
        assert_eq!(
            classify_capture(ProbeChannel::SystemAudio, &[0.0; 4096], 0.0),
            Some(ChannelOutcome::NoAudioDetected)
        );
        assert_eq!(
            classify_capture(ProbeChannel::SystemAudio, &[], 0.0),
            Some(ChannelOutcome::NoAudioDetected)
        );
    }

    #[test]
    fn a_signal_under_the_silence_floor_is_no_audio_on_either_channel() {
        let samples = [0.0004, -0.0009, 0.0002];
        let peak = peak_of(&samples);
        assert_eq!(
            classify_capture(ProbeChannel::Microphone, &samples, peak),
            Some(ChannelOutcome::NoAudioDetected)
        );
        assert_eq!(
            classify_capture(ProbeChannel::SystemAudio, &samples, peak),
            Some(ChannelOutcome::NoAudioDetected)
        );
    }

    #[test]
    fn audio_worth_transcribing_is_not_classified_before_the_model_runs() {
        let samples = [0.4, -0.35, 0.2];
        let peak = peak_of(&samples);
        assert_eq!(
            classify_capture(ProbeChannel::Microphone, &samples, peak),
            None
        );
        assert_eq!(
            classify_capture(ProbeChannel::SystemAudio, &samples, peak),
            None
        );
    }

    #[test]
    fn a_model_that_never_loaded_captures_nothing_on_either_channel() {
        let result = nothing_captured(
            ModelReport::failed(ModelCheckOutcome::Unavailable, None),
            Some("MacBook Pro Microphone (input)"),
            None,
        );

        assert_eq!(result.microphone.outcome, ChannelOutcome::NotRun);
        assert_eq!(result.system_audio.outcome, ChannelOutcome::NotRun);
        assert_eq!(result.microphone.peak_level, 0.0);
        assert_eq!(result.system_audio.peak_level, 0.0);
        assert_eq!(result.microphone.duration_ms, 0);
        assert_eq!(result.system_audio.duration_ms, 0);
        assert_eq!(result.duration_ms, 0);
        assert!(!result.cancelled);
        assert_eq!(
            result.microphone.device_name,
            "MacBook Pro Microphone (input)"
        );
        // Nothing was asked for and nothing was opened, so there is no name to
        // report; the frontend words that case itself.
        assert_eq!(result.system_audio.device_name, "");
    }

    #[test]
    fn result_serialises_with_the_camel_case_keys_the_frontend_reads() {
        let mut microphone = channel_report(ProbeChannel::Microphone, ChannelOutcome::Transcribed);
        microphone.transcript = Some("testing one two three".to_string());

        let result = SetupCheckResult {
            model: ModelReport::loaded(Some(PREFERRED_MODEL.to_string())),
            microphone,
            system_audio: ChannelReport::new(
                ProbeChannel::SystemAudio,
                ChannelOutcome::NoAudioDetected,
                "MacBook Pro Speakers".to_string(),
            )
            .with_capture(0.0, 8000),
            duration_ms: 8000,
            cancelled: false,
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["durationMs"], 8000);
        assert_eq!(json["cancelled"], false);
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("system_audio").is_none());

        assert_eq!(json["model"]["outcome"], "loaded");
        assert_eq!(json["model"]["modelName"], PREFERRED_MODEL);
        assert!(json["model"]["detail"].is_null());
        assert!(json["model"].get("model_name").is_none());

        assert_eq!(json["microphone"]["channel"], "microphone");
        assert_eq!(json["microphone"]["outcome"], "transcribed");
        assert_eq!(json["microphone"]["transcript"], "testing one two three");
        assert_eq!(json["microphone"]["deviceName"], "MacBook Pro Microphone");
        assert_eq!(json["microphone"]["peakLevel"], 0.5);
        assert_eq!(json["microphone"]["durationMs"], 8000);
        assert!(json["microphone"].get("device_name").is_none());
        assert!(json["microphone"].get("peak_level").is_none());

        assert_eq!(json["systemAudio"]["channel"], "system_audio");
        assert_eq!(json["systemAudio"]["outcome"], "no_audio_detected");
        assert!(json["systemAudio"]["transcript"].is_null());
        assert_eq!(json["systemAudio"]["deviceName"], "MacBook Pro Speakers");
        assert_eq!(json["systemAudio"]["peakLevel"], 0.0);
        assert!(json["systemAudio"].get("duration_ms").is_none());
    }

    #[test]
    fn channel_outcome_strings_match_the_frontend_union() {
        // The frontend switches on every one of these by name, so a variant
        // renamed here is a blank screen at runtime rather than a type error.
        let expected = [
            (ChannelOutcome::Transcribed, "transcribed"),
            (ChannelOutcome::NoSpeechDetected, "no_speech_detected"),
            (ChannelOutcome::NoAudioDetected, "no_audio_detected"),
            (ChannelOutcome::PermissionDenied, "permission_denied"),
            (ChannelOutcome::DeviceUnavailable, "device_unavailable"),
            (ChannelOutcome::TranscriptionFailed, "transcription_failed"),
            (ChannelOutcome::Cancelled, "cancelled"),
            (ChannelOutcome::Unsupported, "unsupported"),
            (ChannelOutcome::NotRun, "not_run"),
        ];

        for (outcome, name) in expected {
            assert_eq!(serde_json::to_value(outcome).unwrap(), name);
        }
    }

    #[test]
    fn model_outcome_strings_match_the_frontend_union() {
        let expected = [
            (ModelCheckOutcome::Loaded, "loaded"),
            (ModelCheckOutcome::Unavailable, "unavailable"),
            (ModelCheckOutcome::Failed, "failed"),
        ];

        for (outcome, name) in expected {
            assert_eq!(serde_json::to_value(outcome).unwrap(), name);
        }
    }

    #[test]
    fn a_cancelled_check_keeps_what_each_channel_measured() {
        let report = channel_report(ProbeChannel::SystemAudio, ChannelOutcome::Cancelled);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["outcome"], "cancelled");
        assert_eq!(json["peakLevel"], 0.5);
        assert_eq!(json["durationMs"], 8000);
    }

    #[test]
    fn a_detail_carries_the_error_text_without_becoming_the_outcome() {
        let report = ChannelReport::new(
            ProbeChannel::Microphone,
            ChannelOutcome::DeviceUnavailable,
            "Missing USB Mic".to_string(),
        )
        .with_detail(Some("Device not found".to_string()));

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["outcome"], "device_unavailable");
        assert_eq!(json["detail"], "Device not found");
        assert!(json["transcript"].is_null());
    }
}
