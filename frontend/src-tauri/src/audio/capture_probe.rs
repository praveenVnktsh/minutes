// audio/capture_probe.rs
//
// One channel of the setup check, captured the same way whichever channel it
// is. The microphone and the system-audio tap differ only in where the samples
// come from; the meter the user watches, the length of the window and the
// arithmetic that later calls it silence all have to behave identically for the
// two reports to be comparable. So the setup check asks this module for a
// channel instead of treating system audio as a variation on the microphone.
//
// Nothing here decides an outcome. A probe returns the samples it got, or one
// of the two failures that mean there were never going to be any; the caller
// classifies everything else.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Stream, SupportedStreamConfig};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};

use super::devices::{
    default_input_device, default_output_device, get_device_and_config, parse_audio_device,
    AudioDevice, DeviceType,
};

/// Level updates ride the same 60fps budget as the rest of the app's metering.
const LEVEL_INTERVAL: Duration = Duration::from_millis(33);
const LEVEL_EVENT: &str = "setup-check-level";

/// RMS above this is a signal the user would describe as "the meter moved".
const ACTIVE_RMS_FLOOR: f32 = 0.01;

/// Used when the macOS tap is running but no output device resolves behind it,
/// so the frontend still has something to name in its sentence.
#[cfg(target_os = "macos")]
const SYSTEM_AUDIO_FALLBACK_NAME: &str = "System audio";

/// Which channel a probe is capturing. The string form is what the
/// `setup-check-level` event and `ChannelReport` both carry as `channel`, so it
/// is fixed by the wire contract and not just an internal label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeChannel {
    Microphone,
    SystemAudio,
}

impl ProbeChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeChannel::Microphone => "microphone",
            ProbeChannel::SystemAudio => "system_audio",
        }
    }
}

impl fmt::Display for ProbeChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one channel handed back once its capture was torn down.
pub struct ProbeCapture {
    pub samples: Vec<f32>,
    /// 1 for the macOS system tap, whatever the device reported otherwise.
    pub channels: u16,
    pub sample_rate: u32,
    pub device_name: String,
    pub cancelled: bool,
}

impl ProbeCapture {
    /// How much audio actually arrived, which is what the user is told about —
    /// not how long we sat waiting for it. Interleaved samples are counted as
    /// frames, so a stereo device does not claim twice its real duration.
    pub fn duration_ms(&self) -> u64 {
        let channels = self.channels.max(1) as u64;
        let frames = self.samples.len() as u64 / channels;
        frames * 1000 / self.sample_rate.max(1) as u64
    }
}

/// The two ways a probe can come back with no capture at all. Everything else a
/// channel can be guilty of — silence, no speech, a model that errored — is
/// decided from the samples by the caller.
///
/// `Unsupported` becomes `ChannelOutcome::Unsupported` and `DeviceUnavailable`
/// becomes `ChannelOutcome::DeviceUnavailable`.
#[derive(Debug, Clone)]
pub enum ProbeError {
    /// This platform cannot capture this channel here at all.
    Unsupported(String),
    /// The device resolved to nothing, or would not open.
    DeviceUnavailable(String),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::Unsupported(detail) => f.write_str(detail),
            ProbeError::DeviceUnavailable(detail) => f.write_str(detail),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeLevel {
    /// Two probes emit on this one event name at the same time, so every
    /// payload has to say which meter it belongs to.
    channel: ProbeChannel,
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

/// Capture one channel for `duration`, metering it as it goes.
///
/// Returns as soon as `cancelled` is raised, carrying whatever arrived before
/// that, and never waits longer than `duration`. Both channels of a setup check
/// are meant to run concurrently over one window, so the caller can join two of
/// these and the user waits once.
pub async fn probe_channel<R: Runtime>(
    app: AppHandle<R>,
    channel: ProbeChannel,
    device_name: Option<String>,
    duration: Duration,
    cancelled: Arc<AtomicBool>,
) -> Result<ProbeCapture, ProbeError> {
    match channel {
        ProbeChannel::Microphone => {
            let device = resolve_device(device_name.as_deref(), DeviceType::Input)?;
            probe_with_cpal(app, channel, device, duration, cancelled).await
        }
        ProbeChannel::SystemAudio => {
            probe_system_audio(app, device_name, duration, cancelled).await
        }
    }
}

/// The frontend sends the display form ("MacBook Pro Microphone (input)"), but
/// a bare name is worth trying as this channel's kind of device rather than
/// failing outright.
fn resolve_device(
    device_name: Option<&str>,
    device_type: DeviceType,
) -> Result<AudioDevice, ProbeError> {
    let Some(name) = device_name else {
        let default = match device_type {
            DeviceType::Input => default_input_device(),
            DeviceType::Output => default_output_device(),
        };
        return default.map_err(|error| ProbeError::DeviceUnavailable(error.to_string()));
    };

    Ok(
        parse_audio_device(name)
            .unwrap_or_else(|_| AudioDevice::new(name.to_string(), device_type)),
    )
}

/// On macOS system audio comes off the CoreAudio tap, which is already an async
/// stream of mono samples and needs no cpal device of its own.
#[cfg(target_os = "macos")]
async fn probe_system_audio<R: Runtime>(
    app: AppHandle<R>,
    _device_name: Option<String>,
    duration: Duration,
    cancelled: Arc<AtomicBool>,
) -> Result<ProbeCapture, ProbeError> {
    use futures_util::StreamExt;

    // The tap can be created even when the permission was never granted: it
    // then simply yields silence, which is exactly what the first-run audit
    // records as FR-02. So this probe reports what it measured and leaves the
    // question of a grant alone — a silent tap here is "we heard nothing", not
    // "you are authorised".
    let mut stream = crate::audio::capture::start_system_audio_capture()
        .await
        .map_err(|error| ProbeError::Unsupported(error.to_string()))?;

    let sample_rate = stream.sample_rate().max(1);
    let device_name = default_output_device()
        .map(|device| device.name)
        .unwrap_or_else(|_| SYSTEM_AUDIO_FALLBACK_NAME.to_string());

    info!(
        "Setup check tapping system audio from '{}' - {} Hz for {} ms",
        device_name,
        sample_rate,
        duration.as_millis()
    );

    let started = Instant::now();
    let duration_ms = duration.as_millis() as u64;
    let mut samples: Vec<f32> = Vec::new();
    let mut reported = 0usize;

    let capture = async {
        let mut ticker = tokio::time::interval(LEVEL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                sample = stream.next() => match sample {
                    Some(sample) => samples.push(sample),
                    // The tap closed under us; what we already have is what the
                    // user gets, and its silence will speak for itself.
                    None => return false,
                },
                _ = ticker.tick() => {
                    let level = level_payload(
                        ProbeChannel::SystemAudio,
                        &samples[reported..],
                        started.elapsed(),
                        duration_ms,
                    );
                    reported = samples.len();
                    emit_level(&app, &level);

                    if cancelled.load(Ordering::SeqCst) {
                        return true;
                    }
                }
            }
        }
    };

    // Timing out the whole loop is what keeps the window exact: the tap future
    // is dropped at the await it was parked on, whatever is still pending.
    let stopped_early = tokio::time::timeout(duration, capture)
        .await
        .unwrap_or(false);

    info!(
        "Setup check captured {} system audio samples (cancelled: {})",
        samples.len(),
        stopped_early
    );

    Ok(ProbeCapture {
        samples,
        // The global tap is created mono, so there is nothing to de-interleave.
        channels: 1,
        sample_rate,
        device_name,
        cancelled: stopped_early,
    })
}

/// Everywhere else system audio is an ordinary input stream over an output
/// device: WASAPI loopback on Windows, a PulseAudio monitor source on Linux.
/// `get_device_and_config` already knows which of those it is looking at.
#[cfg(not(target_os = "macos"))]
async fn probe_system_audio<R: Runtime>(
    app: AppHandle<R>,
    device_name: Option<String>,
    duration: Duration,
    cancelled: Arc<AtomicBool>,
) -> Result<ProbeCapture, ProbeError> {
    let device = resolve_device(device_name.as_deref(), DeviceType::Output)?;
    probe_with_cpal(app, ProbeChannel::SystemAudio, device, duration, cancelled).await
}

/// Run the cpal capture on its own thread: `Stream` is not `Send` on every
/// platform and nothing !Send may live across an await in a Tauri command.
async fn probe_with_cpal<R: Runtime>(
    app: AppHandle<R>,
    channel: ProbeChannel,
    device: AudioDevice,
    duration: Duration,
    cancelled: Arc<AtomicBool>,
) -> Result<ProbeCapture, ProbeError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let runtime = tokio::runtime::Handle::current();

    std::thread::Builder::new()
        .name(format!("probe-{}", channel))
        .spawn(move || {
            let _ = sender.send(run_cpal_capture(
                app, channel, device, duration, cancelled, runtime,
            ));
        })
        .map_err(|error| {
            ProbeError::DeviceUnavailable(format!(
                "Failed to start the {} capture thread: {}",
                channel, error
            ))
        })?;

    // A thread that died without answering is reported as an unavailable device
    // rather than propagated as a panic on the caller's task.
    receiver.await.unwrap_or_else(|_| {
        Err(ProbeError::DeviceUnavailable(format!(
            "The {} capture thread ended without a result",
            channel
        )))
    })
}

fn run_cpal_capture<R: Runtime>(
    app: AppHandle<R>,
    channel: ProbeChannel,
    device: AudioDevice,
    duration: Duration,
    cancelled: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
) -> Result<ProbeCapture, ProbeError> {
    // Device lookup is synchronous under its async signature, so borrowing the
    // runtime for it costs the app nothing.
    let (cpal_device, config) = runtime
        .block_on(get_device_and_config(&device))
        .map_err(|error| ProbeError::DeviceUnavailable(error.to_string()))?;

    let channels = config.channels();
    let sample_rate = config.sample_rate().0;
    info!(
        "Setup check capturing {} from '{}' - {} Hz, {} channels, {:?}",
        channel,
        device.name,
        sample_rate,
        channels,
        config.sample_format()
    );

    let buffer = Arc::new(Mutex::new(CaptureBuffer::default()));
    let stream = build_capture_stream(&cpal_device, &config, channel, buffer.clone())
        .map_err(|error| ProbeError::DeviceUnavailable(error.to_string()))?;
    stream
        .play()
        .map_err(|error| ProbeError::DeviceUnavailable(error.to_string()))?;

    let cancelled = emit_levels(&app, channel, &buffer, duration, &cancelled);

    // Pause before dropping so the callback stops touching the buffer first.
    if let Err(error) = stream.pause() {
        warn!(
            "Setup check could not pause the {} stream cleanly: {}",
            channel, error
        );
    }
    drop(stream);

    let captured = std::mem::take(
        &mut *buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    info!(
        "Setup check captured {} {} samples over {} callbacks (cancelled: {})",
        captured.samples.len(),
        channel,
        captured.callbacks,
        cancelled
    );

    Ok(ProbeCapture {
        samples: captured.samples,
        channels,
        sample_rate,
        device_name: device.name,
        cancelled,
    })
}

fn build_capture_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    channel: ProbeChannel,
    buffer: Arc<Mutex<CaptureBuffer>>,
) -> anyhow::Result<Stream> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let on_error =
        move |error: cpal::StreamError| error!("Setup check {} stream error: {}", channel, error);

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
/// arrived since the last one. Returns true if the user stopped the check.
fn emit_levels<R: Runtime>(
    app: &AppHandle<R>,
    channel: ProbeChannel,
    buffer: &Mutex<CaptureBuffer>,
    duration: Duration,
    cancelled: &AtomicBool,
) -> bool {
    let started = Instant::now();
    let duration_ms = duration.as_millis() as u64;
    let mut reported = 0usize;

    loop {
        // Sleeping no further than the deadline matters here in a way it did
        // not for a check with one channel: this probe shares its window with
        // the other one, and the user waits for whichever finishes last.
        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(LEVEL_INTERVAL.min(remaining));

        // The lock is released before emitting so the audio callback is never
        // blocked by the IPC hop.
        let level = {
            let buffer = buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let level = level_payload(
                channel,
                &buffer.samples[reported..],
                started.elapsed(),
                duration_ms,
            );
            reported = buffer.samples.len();
            level
        };
        emit_level(app, &level);

        if cancelled.load(Ordering::SeqCst) {
            return true;
        }
    }
}

fn level_payload(
    channel: ProbeChannel,
    window: &[f32],
    elapsed: Duration,
    duration_ms: u64,
) -> ProbeLevel {
    let rms = rms_of(window);
    ProbeLevel {
        channel,
        rms: rms.min(1.0),
        peak: peak_of(window).min(1.0),
        is_active: rms > ACTIVE_RMS_FLOOR,
        elapsed_ms: (elapsed.as_millis() as u64).min(duration_ms),
        duration_ms,
    }
}

fn emit_level<R: Runtime>(app: &AppHandle<R>, level: &ProbeLevel) {
    if let Err(error) = app.emit(LEVEL_EVENT, level) {
        warn!(
            "Failed to emit a {} setup check level: {}",
            level.channel, error
        );
    }
}

pub fn peak_of(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

pub fn rms_of(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|sample| sample * sample).sum();
    (sum / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(samples: Vec<f32>, channels: u16, sample_rate: u32) -> ProbeCapture {
        ProbeCapture {
            samples,
            channels,
            sample_rate,
            device_name: "MacBook Pro Microphone".to_string(),
            cancelled: false,
        }
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
        assert_eq!(capture(vec![0.0; 32_000], 2, 16_000).duration_ms(), 1000);
        assert_eq!(capture(vec![0.0; 16_000], 1, 16_000).duration_ms(), 1000);
    }

    #[test]
    fn captured_duration_survives_a_device_that_reported_nothing() {
        assert_eq!(capture(Vec::new(), 0, 0).duration_ms(), 0);
    }

    #[test]
    fn channel_names_match_the_wire_contract() {
        assert_eq!(ProbeChannel::Microphone.as_str(), "microphone");
        assert_eq!(ProbeChannel::SystemAudio.as_str(), "system_audio");
        assert_eq!(ProbeChannel::SystemAudio.to_string(), "system_audio");
        assert_eq!(
            serde_json::to_value(ProbeChannel::Microphone).unwrap(),
            "microphone"
        );
        assert_eq!(
            serde_json::to_value(ProbeChannel::SystemAudio).unwrap(),
            "system_audio"
        );
    }

    #[test]
    fn level_payload_serialises_with_camel_case_keys() {
        let level = level_payload(
            ProbeChannel::SystemAudio,
            &[0.5, -0.5],
            Duration::from_millis(1320),
            8000,
        );

        let json = serde_json::to_value(&level).unwrap();
        assert_eq!(json["channel"], "system_audio");
        assert_eq!(json["peak"], 0.5);
        assert_eq!(json["isActive"], true);
        assert_eq!(json["elapsedMs"], 1320);
        assert_eq!(json["durationMs"], 8000);
        assert!(json.get("is_active").is_none());
        assert!(json.get("elapsed_ms").is_none());
    }

    #[test]
    fn a_quiet_window_is_not_reported_as_an_active_meter() {
        let level = level_payload(
            ProbeChannel::Microphone,
            &[0.0004, -0.0009],
            Duration::from_millis(33),
            8000,
        );
        assert!(!level.is_active);
    }

    #[test]
    fn elapsed_never_runs_past_the_window_it_is_drawn_in() {
        let level = level_payload(
            ProbeChannel::Microphone,
            &[],
            Duration::from_millis(8040),
            8000,
        );
        assert_eq!(level.elapsed_ms, 8000);
    }
}
