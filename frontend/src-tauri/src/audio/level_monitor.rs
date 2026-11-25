// audio/level_monitor.rs
//
// The input-level meter behind the device picker's "Test Mic" button
// (Settings › Recording). It opens the chosen microphone with cpal, measures
// what actually arrives, and emits an `audio-levels` update every
// EMIT_INTERVAL until the picker stops it.
//
// It never competes with a recording for the microphone: it refuses to start
// while one is running or stopping, the start-recording commands stop it
// before they open the mic, and the meter loop also bails out on its own as
// soon as it sees a recording.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Stream, SupportedStreamConfig};
use log::{error, info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use super::devices::{
    default_input_device, get_device_and_config, parse_audio_device, AudioDevice, DeviceType,
};

const LEVEL_EVENT: &str = "audio-levels";
const EMIT_INTERVAL: Duration = Duration::from_millis(100);

/// RMS above this is a signal the user would describe as "the meter moved"
/// (the same floor the setup check uses).
const ACTIVE_RMS_FLOOR: f32 = 0.01;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct AudioLevelData {
    pub device_name: String,
    pub device_type: String, // always "input": only microphones are metered
    pub rms_level: f32,      // RMS level (0.0 to 1.0)
    pub peak_level: f32,     // Peak level (0.0 to 1.0)
    pub is_active: bool,     // Whether audio is being detected
}

#[derive(Debug, Serialize, Clone)]
pub struct AudioLevelUpdate {
    pub timestamp: u64,
    pub levels: Vec<AudioLevelData>,
}

/// Bumped by every start and stop. A meter thread runs only while the
/// generation it was started with is still current, so a restart (a different
/// microphone picked) or a stop retires the old thread without a handle to it.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static MONITORING: AtomicBool = AtomicBool::new(false);

/// What arrived from the device since the meter last reported.
#[derive(Debug, Default)]
struct LevelWindow {
    sum_squares: f64,
    samples: usize,
    peak: f32,
}

impl LevelWindow {
    fn add(&mut self, data: impl IntoIterator<Item = f32>) {
        for sample in data {
            self.sum_squares += f64::from(sample) * f64::from(sample);
            self.samples += 1;
            self.peak = self.peak.max(sample.abs());
        }
    }

    /// The window's level, clamped to 0.0-1.0, and a fresh window. A window
    /// nothing arrived in reads as silence.
    fn take(&mut self, device_name: &str) -> AudioLevelData {
        let window = std::mem::take(self);
        let rms = if window.samples == 0 {
            0.0
        } else {
            (window.sum_squares / window.samples as f64).sqrt() as f32
        };
        AudioLevelData {
            device_name: device_name.to_string(),
            device_type: "input".to_string(),
            rms_level: rms.min(1.0),
            peak_level: window.peak.min(1.0),
            is_active: rms > ACTIVE_RMS_FLOOR,
        }
    }
}

/// Start metering `device_name` (the picker's "<name> (input)" form, or None for
/// the default microphone), replacing whatever was being metered. Returns once
/// the device is open, or with why it could not be.
pub async fn start_monitoring<R: Runtime>(
    app_handle: AppHandle<R>,
    device_name: Option<String>,
) -> Result<()> {
    if super::recording_commands::is_recording_or_stopping() {
        return Err(anyhow!("The microphone is in use by the current recording"));
    }

    let device = resolve_input_device(device_name.as_deref())?;
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    MONITORING.store(true, Ordering::SeqCst);
    info!("Starting audio level monitoring for '{}'", device.name);

    let (opened, opened_receiver) = tokio::sync::oneshot::channel();
    let runtime = tokio::runtime::Handle::current();
    // cpal's Stream is not Send, so it lives and dies on its own thread.
    std::thread::Builder::new()
        .name("audio-level-monitor".to_string())
        .spawn(move || run_meter(app_handle, device, generation, runtime, opened))
        .map_err(|error| anyhow!("Failed to start the level monitor thread: {}", error))?;

    let result = opened_receiver.await.unwrap_or_else(|_| {
        Err(anyhow!(
            "The level monitor thread ended without opening the device"
        ))
    });
    if result.is_err() && GENERATION.load(Ordering::SeqCst) == generation {
        MONITORING.store(false, Ordering::SeqCst);
    }
    result
}

/// Stop metering. The meter thread closes the device within one EMIT_INTERVAL.
pub fn stop_monitoring() {
    if MONITORING.swap(false, Ordering::SeqCst) {
        info!("Stopping audio level monitoring");
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

pub fn is_monitoring() -> bool {
    MONITORING.load(Ordering::SeqCst)
}

fn resolve_input_device(device_name: Option<&str>) -> Result<AudioDevice> {
    match device_name {
        None => default_input_device(),
        Some(name) => Ok(parse_audio_device(name)
            .unwrap_or_else(|_| AudioDevice::new(name.to_string(), DeviceType::Input))),
    }
}

fn run_meter<R: Runtime>(
    app: AppHandle<R>,
    device: AudioDevice,
    generation: u64,
    runtime: tokio::runtime::Handle,
    opened: tokio::sync::oneshot::Sender<Result<()>>,
) {
    let window = Arc::new(Mutex::new(LevelWindow::default()));
    // Device lookup is synchronous under its async signature.
    let stream = runtime
        .block_on(get_device_and_config(&device))
        .and_then(|(cpal_device, config)| build_level_stream(&cpal_device, &config, window.clone()))
        .and_then(|stream| {
            stream.play()?;
            Ok(stream)
        });
    let stream = match stream {
        Ok(stream) => {
            let _ = opened.send(Ok(()));
            stream
        }
        Err(error) => {
            warn!(
                "Could not open '{}' for level monitoring: {}",
                device.name, error
            );
            let _ = opened.send(Err(error));
            return;
        }
    };

    while GENERATION.load(Ordering::SeqCst) == generation {
        std::thread::sleep(EMIT_INTERVAL);
        if GENERATION.load(Ordering::SeqCst) != generation {
            break;
        }
        if super::recording_commands::is_recording_or_stopping() {
            info!("A recording started; stopping audio level monitoring");
            break;
        }
        let level = window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take(&device.name);
        let update = AudioLevelUpdate {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            levels: vec![level],
        };
        if let Err(error) = app.emit(LEVEL_EVENT, &update) {
            error!("Failed to emit audio levels: {}", error);
            break;
        }
    }

    // Pause before dropping so the callback stops touching the window first.
    if let Err(error) = stream.pause() {
        warn!(
            "Could not pause the level monitor stream cleanly: {}",
            error
        );
    }
    drop(stream);
    if GENERATION.load(Ordering::SeqCst) == generation {
        MONITORING.store(false, Ordering::SeqCst);
    }
    info!("Audio level monitoring for '{}' ended", device.name);
}

fn build_level_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    window: Arc<Mutex<LevelWindow>>,
) -> Result<Stream> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let on_error = |error: cpal::StreamError| error!("Level monitor stream error: {}", error);
    let add = move |data: &mut dyn Iterator<Item = f32>| {
        window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .add(data);
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| add(&mut data.iter().copied()),
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                add(&mut data.iter().map(|&sample| sample as f32 / i16::MAX as f32))
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _: &cpal::InputCallbackInfo| {
                add(&mut data.iter().map(|&sample| sample as f32 / i32::MAX as f32))
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I8 => device.build_input_stream(
            &stream_config,
            move |data: &[i8], _: &cpal::InputCallbackInfo| {
                add(&mut data.iter().map(|&sample| sample as f32 / i8::MAX as f32))
            },
            on_error,
            None,
        )?,
        format => return Err(anyhow!("Unsupported sample format: {:?}", format)),
    };
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reports_the_rms_and_peak_of_what_arrived_across_callbacks() {
        let mut window = LevelWindow::default();
        window.add([0.5, -0.5]);
        window.add([0.5, -0.5]);
        let level = window.take("Mic");
        assert!((level.rms_level - 0.5).abs() < 1e-6);
        assert!((level.peak_level - 0.5).abs() < 1e-6);
        assert!(level.is_active);
        assert_eq!(level.device_name, "Mic");
        assert_eq!(level.device_type, "input");
    }

    #[test]
    fn taking_a_window_starts_a_fresh_one() {
        let mut window = LevelWindow::default();
        window.add([0.8, -0.8]);
        window.take("Mic");
        window.add([0.1]);
        let level = window.take("Mic");
        assert!((level.rms_level - 0.1).abs() < 1e-6);
        assert!((level.peak_level - 0.1).abs() < 1e-6);
    }

    #[test]
    fn a_window_nothing_arrived_in_is_silence() {
        let level = LevelWindow::default().take("Mic");
        assert_eq!(level.rms_level, 0.0);
        assert_eq!(level.peak_level, 0.0);
        assert!(!level.is_active);
    }

    #[test]
    fn a_quiet_window_is_not_active_and_a_clipping_one_is_clamped() {
        let mut quiet = LevelWindow::default();
        quiet.add([0.005, -0.005]);
        assert!(!quiet.take("Mic").is_active);

        let mut loud = LevelWindow::default();
        loud.add([1.5, -2.0]);
        let level = loud.take("Mic");
        assert_eq!(level.rms_level, 1.0);
        assert_eq!(level.peak_level, 1.0);
    }
}
