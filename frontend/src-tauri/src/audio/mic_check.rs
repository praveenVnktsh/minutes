// audio/mic_check.rs
//
// The microphone check that closes onboarding: record a few seconds from one
// input device, show the user a meter driven by their own voice, and prove the
// downloaded Parakeet model can turn it into text. Every way this can go wrong
// gets its own outcome, because the frontend has a specific fix to offer for
// each one and a generic failure here teaches the user nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Stream, SupportedStreamConfig};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};

use super::audio_processing::{audio_to_mono, resample_audio};
use super::devices::{
    default_input_device, get_device_and_config, parse_audio_device, AudioDevice, DeviceType,
};
use crate::parakeet_engine::commands::{parakeet_init, PARAKEET_ENGINE};
use crate::parakeet_engine::{ModelStatus, ParakeetEngine};

/// Level updates ride the same 60fps budget as the rest of the app's metering.
const LEVEL_INTERVAL: Duration = Duration::from_millis(33);
const LEVEL_EVENT: &str = "mic-check-level";

const DEFAULT_DURATION_SECS: f64 = 8.0;
const MIN_DURATION_SECS: f64 = 3.0;
const MAX_DURATION_SECS: f64 = 15.0;

/// A peak under this is indistinguishable from a muted device or an input
/// nobody is talking into — roughly -40 dBFS.
const SILENCE_FLOOR: f32 = 0.01;
/// RMS above this is a signal the user would describe as "the meter moved".
const ACTIVE_RMS_FLOOR: f32 = 0.01;

/// Parakeet wants mono f32 at this rate.
const TRANSCRIBE_SAMPLE_RATE: u32 = 16_000;
/// The model onboarding downloads, and so the one most likely to be on disk.
const PREFERRED_MODEL: &str = "parakeet-tdt-0.6b-v3-int8";

/// Stands in for a device we never managed to open, so the frontend still has
/// something to name in its message.
const UNNAMED_DEVICE: &str = "the default microphone";

/// Only one check may own the input device at a time.
static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// Raised by `mic_check_cancel`, cleared when a check takes the flag above.
static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Everything the frontend can react to. Note that `command_failed` from the
/// TypeScript union has no variant here: it is synthesized in the frontend when
/// the invoke itself throws, which is precisely the case Rust cannot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicCheckOutcome {
    Transcribed,
    NoSpeechDetected,
    NoAudioDetected,
    PermissionDenied,
    DeviceUnavailable,
    ModelUnavailable,
    ModelFailed,
    TranscriptionFailed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicCheckResult {
    pub outcome: MicCheckOutcome,
    /// Only set when the check actually produced words.
    pub transcript: Option<String>,
    pub device_name: String,
    pub peak_level: f32,
    pub duration_ms: u64,
    /// The underlying error, for the log and a details line. Never the whole
    /// message shown to the user — the outcome decides that.
    pub detail: Option<String>,
}

impl MicCheckResult {
    fn new(outcome: MicCheckOutcome, device_name: String) -> Self {
        Self {
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MicCheckLevel {
    rms: f32,
    peak: f32,
    is_active: bool,
    elapsed_ms: u64,
    duration_ms: u64,
}

/// Samples as they arrive, plus enough bookkeeping to tell "the device gave us
/// nothing" apart from "the device gave us silence".
#[derive(Default)]
struct CaptureBuffer {
    samples: Vec<f32>,
    callbacks: u64,
}

/// What the capture thread hands back once the stream is torn down.
struct Capture {
    samples: Vec<f32>,
    channels: u16,
    sample_rate: u32,
    cancelled: bool,
}

/// Clears `CHECK_IN_FLIGHT` however the command exits.
struct InFlightGuard;

impl InFlightGuard {
    fn acquire() -> Option<Self> {
        CHECK_IN_FLIGHT
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| {
                // A cancel that arrived between two checks must not kill this one.
                CANCEL_REQUESTED.store(false, Ordering::SeqCst);
                Self
            })
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        CANCEL_REQUESTED.store(false, Ordering::SeqCst);
        CHECK_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// Record from one input device, transcribe what came back, and report which of
/// the many ways this can fail actually happened.
#[tauri::command]
pub async fn mic_check_start<R: Runtime>(
    app: AppHandle<R>,
    device_name: Option<String>,
    duration_secs: Option<f64>,
) -> Result<MicCheckResult, String> {
    let _guard = match InFlightGuard::acquire() {
        Some(guard) => guard,
        None => return Err("A microphone check is already running".to_string()),
    };

    let duration = capture_duration(duration_secs);
    let device = match resolve_device(device_name.as_deref()) {
        Ok(device) => device,
        Err(error) => {
            warn!("Mic check could not resolve an input device: {}", error);
            let name = device_name.unwrap_or_else(|| UNNAMED_DEVICE.to_string());
            return Ok(
                MicCheckResult::new(MicCheckOutcome::DeviceUnavailable, name)
                    .with_detail(Some(error)),
            );
        }
    };

    info!(
        "Mic check starting on '{}' for {} ms",
        device.name,
        duration.as_millis()
    );

    let capture = match capture_audio(app, device.clone(), duration).await? {
        Ok(capture) => capture,
        Err(error) => {
            error!("Mic check could not open '{}': {}", device.name, error);
            return Ok(
                MicCheckResult::new(MicCheckOutcome::DeviceUnavailable, device.name)
                    .with_detail(Some(error)),
            );
        }
    };

    let peak = peak_of(&capture.samples);
    let duration_ms = captured_duration_ms(&capture);
    let result =
        |outcome| MicCheckResult::new(outcome, device.name.clone()).with_capture(peak, duration_ms);

    if capture.cancelled {
        info!("Mic check cancelled after {} ms of audio", duration_ms);
        return Ok(result(MicCheckOutcome::Cancelled));
    }

    // Silence is diagnosed from the samples alone; there is nothing in it for
    // the model to work with and its verdict would only muddy the message.
    if let Some(outcome) = classify_capture(&capture.samples, peak) {
        warn!(
            "Mic check on '{}' captured no usable audio: {:?} (peak {:.4}, {} ms)",
            device.name, outcome, peak, duration_ms
        );
        return Ok(result(outcome));
    }

    let engine = match ready_engine().await {
        Ok(engine) => engine,
        Err((outcome, detail)) => {
            warn!(
                "Mic check has no usable Parakeet model: {:?} ({:?})",
                outcome, detail
            );
            return Ok(result(outcome).with_detail(detail));
        }
    };

    if CANCEL_REQUESTED.load(Ordering::SeqCst) {
        return Ok(result(MicCheckOutcome::Cancelled));
    }

    let mono = audio_to_mono(&capture.samples, capture.channels);
    let resampled = resample_audio(&mono, capture.sample_rate, TRANSCRIBE_SAMPLE_RATE);

    let transcript = match engine.transcribe_audio(resampled).await {
        Ok(transcript) => transcript,
        Err(error) => {
            error!("Mic check transcription failed: {}", error);
            return Ok(
                result(MicCheckOutcome::TranscriptionFailed).with_detail(Some(error.to_string()))
            );
        }
    };

    let outcome = classify(&capture.samples, peak, &transcript);
    info!(
        "Mic check on '{}' finished: {:?} ({} ms, peak {:.4})",
        device.name, outcome, duration_ms, peak
    );

    let mut final_result = result(outcome);
    if outcome == MicCheckOutcome::Transcribed {
        final_result.transcript = Some(transcript.trim().to_string());
    }
    Ok(final_result)
}

/// Stop a capture that is still running; the pending `mic_check_start` then
/// resolves with `cancelled`.
#[tauri::command]
pub async fn mic_check_cancel() -> Result<(), String> {
    if CHECK_IN_FLIGHT.load(Ordering::SeqCst) {
        info!("Mic check cancellation requested");
        CANCEL_REQUESTED.store(true, Ordering::SeqCst);
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

/// The frontend sends the display form ("MacBook Pro Microphone (input)"), but
/// a bare device name is worth trying as an input rather than failing outright.
fn resolve_device(device_name: Option<&str>) -> Result<AudioDevice, String> {
    match device_name {
        Some(name) => Ok(parse_audio_device(name)
            .unwrap_or_else(|_| AudioDevice::new(name.to_string(), DeviceType::Input))),
        None => default_input_device().map_err(|error| error.to_string()),
    }
}

/// Run the cpal capture on its own thread: `Stream` is not `Send` on every
/// platform and nothing !Send may live across an await in a Tauri command.
///
/// The outer `Result` is a thread that died on us; the inner one is a device
/// that would not open.
async fn capture_audio<R: Runtime>(
    app: AppHandle<R>,
    device: AudioDevice,
    duration: Duration,
) -> Result<Result<Capture, String>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let runtime = tokio::runtime::Handle::current();

    std::thread::Builder::new()
        .name("mic-check-capture".to_string())
        .spawn(move || {
            let _ = sender.send(run_capture(app, device, duration, runtime));
        })
        .map_err(|error| format!("Failed to start microphone check thread: {}", error))?;

    receiver
        .await
        .map_err(|_| "Microphone check thread ended without a result".to_string())
}

fn run_capture<R: Runtime>(
    app: AppHandle<R>,
    device: AudioDevice,
    duration: Duration,
    runtime: tokio::runtime::Handle,
) -> Result<Capture, String> {
    // Device lookup is synchronous under its async signature, so borrowing the
    // runtime for it costs the app nothing.
    let (cpal_device, config) = runtime
        .block_on(get_device_and_config(&device))
        .map_err(|error| error.to_string())?;

    let channels = config.channels();
    let sample_rate = config.sample_rate().0;
    info!(
        "Mic check capturing from '{}' - {} Hz, {} channels, {:?}",
        device.name,
        sample_rate,
        channels,
        config.sample_format()
    );

    let buffer = Arc::new(Mutex::new(CaptureBuffer::default()));
    let stream = build_capture_stream(&cpal_device, &config, buffer.clone())
        .map_err(|error| error.to_string())?;
    stream.play().map_err(|error| error.to_string())?;

    let cancelled = emit_levels(&app, &buffer, duration);

    // Pause before dropping so the callback stops touching the buffer first.
    if let Err(error) = stream.pause() {
        warn!("Mic check could not pause the stream cleanly: {}", error);
    }
    drop(stream);

    let captured = std::mem::take(&mut *buffer.lock().unwrap());
    info!(
        "Mic check captured {} samples over {} callbacks (cancelled: {})",
        captured.samples.len(),
        captured.callbacks,
        cancelled
    );

    Ok(Capture {
        samples: captured.samples,
        channels,
        sample_rate,
        cancelled,
    })
}

fn build_capture_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    buffer: Arc<Mutex<CaptureBuffer>>,
) -> anyhow::Result<Stream> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let on_error = |error: cpal::StreamError| error!("Mic check stream error: {}", error);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                collect_samples(&buffer, data.iter().copied());
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                collect_samples(
                    &buffer,
                    data.iter().map(|&sample| sample as f32 / i16::MAX as f32),
                );
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _: &cpal::InputCallbackInfo| {
                collect_samples(
                    &buffer,
                    data.iter().map(|&sample| sample as f32 / i32::MAX as f32),
                );
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I8 => device.build_input_stream(
            &stream_config,
            move |data: &[i8], _: &cpal::InputCallbackInfo| {
                collect_samples(
                    &buffer,
                    data.iter().map(|&sample| sample as f32 / i8::MAX as f32),
                );
            },
            on_error,
            None,
        )?,
        format => return Err(anyhow::anyhow!("Unsupported sample format: {:?}", format)),
    };

    Ok(stream)
}

/// The audio callback only accumulates; the level loop does the arithmetic.
fn collect_samples(buffer: &Mutex<CaptureBuffer>, data: impl IntoIterator<Item = f32>) {
    let mut buffer = match buffer.lock() {
        Ok(buffer) => buffer,
        Err(poisoned) => poisoned.into_inner(),
    };
    buffer.callbacks += 1;
    buffer.samples.extend(data);
}

/// Wait out the capture, publishing a level for every window of samples that
/// arrived since the last one. Returns true if the user cancelled.
fn emit_levels<R: Runtime>(
    app: &AppHandle<R>,
    buffer: &Mutex<CaptureBuffer>,
    duration: Duration,
) -> bool {
    let started = Instant::now();
    let duration_ms = duration.as_millis() as u64;
    let mut reported = 0usize;

    while started.elapsed() < duration {
        std::thread::sleep(LEVEL_INTERVAL);

        let (rms, peak, total) = {
            let buffer = buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let window = &buffer.samples[reported..];
            (rms_of(window), peak_of(window), buffer.samples.len())
        };
        reported = total;

        let level = MicCheckLevel {
            rms: rms.min(1.0),
            peak: peak.min(1.0),
            is_active: rms > ACTIVE_RMS_FLOOR,
            elapsed_ms: (started.elapsed().as_millis() as u64).min(duration_ms),
            duration_ms,
        };
        if let Err(error) = app.emit(LEVEL_EVENT, &level) {
            warn!("Failed to emit mic check level: {}", error);
        }

        if CANCEL_REQUESTED.load(Ordering::SeqCst) {
            return true;
        }
    }

    false
}

/// How much audio actually arrived, which is what the user is told about — not
/// how long we sat waiting for it.
fn captured_duration_ms(capture: &Capture) -> u64 {
    let channels = capture.channels.max(1) as u64;
    let frames = capture.samples.len() as u64 / channels;
    frames * 1000 / capture.sample_rate.max(1) as u64
}

fn peak_of(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

fn rms_of(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|sample| sample * sample).sum();
    (sum / samples.len() as f32).sqrt()
}

/// macOS hands a denied app a stream of digital silence instead of refusing to
/// open it, so bit-exact zeroes there indict the permission, not the device.
fn digital_silence_outcome() -> MicCheckOutcome {
    #[cfg(target_os = "macos")]
    {
        MicCheckOutcome::PermissionDenied
    }
    #[cfg(not(target_os = "macos"))]
    {
        MicCheckOutcome::NoAudioDetected
    }
}

/// The verdict the captured audio alone supports. `None` means it is worth
/// sending to the model, which then decides between speech and silence.
fn classify_capture(samples: &[f32], peak: f32) -> Option<MicCheckOutcome> {
    if samples.is_empty() {
        return Some(MicCheckOutcome::PermissionDenied);
    }
    if samples.iter().all(|sample| *sample == 0.0) {
        return Some(digital_silence_outcome());
    }
    if peak < SILENCE_FLOOR {
        return Some(MicCheckOutcome::NoAudioDetected);
    }
    None
}

/// The whole classification, once the model has had its say.
fn classify(samples: &[f32], peak: f32, transcript: &str) -> MicCheckOutcome {
    classify_capture(samples, peak).unwrap_or_else(|| {
        if transcript.trim().is_empty() {
            MicCheckOutcome::NoSpeechDetected
        } else {
            MicCheckOutcome::Transcribed
        }
    })
}

fn parakeet_engine() -> Option<Arc<ParakeetEngine>> {
    PARAKEET_ENGINE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .cloned()
}

/// Bring the shared Parakeet engine up with a model loaded, or say which kind
/// of model problem the user has.
async fn ready_engine() -> Result<Arc<ParakeetEngine>, (MicCheckOutcome, Option<String>)> {
    if parakeet_engine().is_none() {
        parakeet_init()
            .await
            .map_err(|error| (MicCheckOutcome::ModelFailed, Some(error)))?;
    }

    let engine = parakeet_engine().ok_or((
        MicCheckOutcome::ModelFailed,
        Some("Parakeet engine not initialized".to_string()),
    ))?;

    if engine.is_model_loaded().await {
        return Ok(engine);
    }

    let models = engine
        .discover_models()
        .await
        .map_err(|error| (MicCheckOutcome::ModelFailed, Some(error.to_string())))?;
    let available: Vec<_> = models
        .iter()
        .filter(|model| matches!(model.status, ModelStatus::Available))
        .collect();

    let model = available
        .iter()
        .find(|model| model.name == PREFERRED_MODEL)
        .or_else(|| available.first())
        .ok_or((MicCheckOutcome::ModelUnavailable, None))?;

    info!("Mic check loading Parakeet model '{}'", model.name);
    engine
        .load_model(&model.name)
        .await
        .map_err(|error| (MicCheckOutcome::ModelFailed, Some(error.to_string())))?;

    Ok(engine)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn no_samples_at_all_means_the_os_withheld_the_audio() {
        assert_eq!(
            classify(&[], 0.0, "anything"),
            MicCheckOutcome::PermissionDenied
        );
    }

    #[test]
    fn digital_silence_indicts_the_permission_on_macos_only() {
        let outcome = classify(&[0.0; 4096], 0.0, "");

        #[cfg(target_os = "macos")]
        assert_eq!(outcome, MicCheckOutcome::PermissionDenied);

        #[cfg(not(target_os = "macos"))]
        assert_eq!(outcome, MicCheckOutcome::NoAudioDetected);
    }

    #[test]
    fn a_signal_under_the_silence_floor_is_no_audio() {
        let samples = [0.0004, -0.0009, 0.0002];
        assert_eq!(
            classify(&samples, peak_of(&samples), ""),
            MicCheckOutcome::NoAudioDetected
        );
    }

    #[test]
    fn audible_audio_with_an_empty_transcript_is_no_speech() {
        let samples = [0.4, -0.35, 0.2];
        assert_eq!(
            classify(&samples, peak_of(&samples), "   \n"),
            MicCheckOutcome::NoSpeechDetected
        );
    }

    #[test]
    fn audible_audio_with_words_passes() {
        let samples = [0.4, -0.35, 0.2];
        assert_eq!(
            classify(&samples, peak_of(&samples), "testing one two three"),
            MicCheckOutcome::Transcribed
        );
    }

    #[test]
    fn audio_worth_transcribing_is_not_classified_before_the_model_runs() {
        let samples = [0.4, -0.35, 0.2];
        assert_eq!(classify_capture(&samples, peak_of(&samples)), None);
    }

    #[test]
    fn result_serialises_with_the_camel_case_keys_the_frontend_reads() {
        let result = MicCheckResult {
            outcome: MicCheckOutcome::Transcribed,
            transcript: Some("testing one two three".to_string()),
            device_name: "MacBook Pro Microphone".to_string(),
            peak_level: 0.5,
            duration_ms: 8000,
            detail: None,
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["outcome"], "transcribed");
        assert_eq!(json["transcript"], "testing one two three");
        assert_eq!(json["deviceName"], "MacBook Pro Microphone");
        assert_eq!(json["peakLevel"], 0.5);
        assert_eq!(json["durationMs"], 8000);
        assert!(json["detail"].is_null());
        assert!(json.get("device_name").is_none());
        assert!(json.get("peak_level").is_none());
        assert!(json.get("duration_ms").is_none());
    }

    #[test]
    fn outcome_strings_match_the_frontend_union() {
        let expected = [
            (MicCheckOutcome::Transcribed, "transcribed"),
            (MicCheckOutcome::NoSpeechDetected, "no_speech_detected"),
            (MicCheckOutcome::NoAudioDetected, "no_audio_detected"),
            (MicCheckOutcome::PermissionDenied, "permission_denied"),
            (MicCheckOutcome::DeviceUnavailable, "device_unavailable"),
            (MicCheckOutcome::ModelUnavailable, "model_unavailable"),
            (MicCheckOutcome::ModelFailed, "model_failed"),
            (MicCheckOutcome::TranscriptionFailed, "transcription_failed"),
            (MicCheckOutcome::Cancelled, "cancelled"),
        ];

        for (outcome, name) in expected {
            assert_eq!(serde_json::to_value(outcome).unwrap(), name);
        }
    }

    #[test]
    fn level_payload_serialises_with_camel_case_keys() {
        let level = MicCheckLevel {
            rms: 0.25,
            peak: 0.75,
            is_active: true,
            elapsed_ms: 1320,
            duration_ms: 8000,
        };

        let json = serde_json::to_value(&level).unwrap();
        assert_eq!(json["rms"], 0.25);
        assert_eq!(json["peak"], 0.75);
        assert_eq!(json["isActive"], true);
        assert_eq!(json["elapsedMs"], 1320);
        assert_eq!(json["durationMs"], 8000);
        assert!(json.get("is_active").is_none());
    }

    #[test]
    fn levels_are_measured_from_the_samples_that_arrived() {
        assert_eq!(peak_of(&[0.1, -0.7, 0.3]), 0.7);
        assert_eq!(peak_of(&[]), 0.0);
        assert_eq!(rms_of(&[]), 0.0);
        assert!((rms_of(&[0.5, -0.5]) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn captured_duration_counts_frames_not_samples() {
        let capture = Capture {
            samples: vec![0.0; 32_000],
            channels: 2,
            sample_rate: 16_000,
            cancelled: false,
        };
        assert_eq!(captured_duration_ms(&capture), 1000);
    }
}
