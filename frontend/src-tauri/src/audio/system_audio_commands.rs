use crate::audio::{new_system_audio_callback, SystemAudioDetector, SystemAudioEvent};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter, State};

// Global state for system audio detector
type SystemAudioDetectorState = Arc<Mutex<Option<SystemAudioDetector>>>;

/// Start monitoring system audio usage by other applications
#[command]
pub async fn start_system_audio_monitoring(
    app_handle: AppHandle,
    detector_state: State<'_, SystemAudioDetectorState>,
) -> Result<(), String> {
    let mut detector_guard = detector_state
        .lock()
        .map_err(|e| format!("Failed to acquire detector lock: {}", e))?;

    if detector_guard.is_some() {
        return Ok(());
    }

    // Warm the ignore list cache before the first event arrives.
    if let Err(error) = crate::meeting_detection::load_settings(&app_handle) {
        tracing::warn!("Meeting detection: could not load settings: {error}");
    }
    crate::meeting_detection::restore_snooze(&app_handle);

    let mut detector = SystemAudioDetector::new();

    // Create callback that emits events to the frontend
    let callback = new_system_audio_callback(move |event| {
        match event {
            SystemAudioEvent::SystemAudioStarted(apps) => {
                tracing::info!("Microphone in use by apps: {:?}", apps);

                // Drive the tray and the floating prompt straight from Rust so
                // they work even if the webview is hidden/throttled. The ignore
                // list, known-only toggle and snooze are applied here.
                let detected = crate::meeting_detection::on_active_apps(&apps);
                crate::tray::set_meeting_detected(
                    &app_handle,
                    detected.as_ref().map(|d| d.display_name.clone()),
                );

                if let Some(detected) = detected {
                    let app = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::meeting_prompt::maybe_show(&app, &detected).await;
                    });
                }

                let names: Vec<String> = apps.into_iter().map(|app| app.name).collect();
                let _ = app_handle.emit("system-audio-started", names);
            }
            SystemAudioEvent::SystemAudioStopped => {
                crate::meeting_detection::on_audio_stopped();
                crate::tray::set_meeting_detected(&app_handle, None);
                crate::meeting_prompt::reset();
                let _ = app_handle.emit("system-audio-stopped", ());
                tracing::info!("System audio stopped");
            }
        }
    });

    detector.start(callback);
    *detector_guard = Some(detector);

    Ok(())
}

/// Initialize the system audio detector state in Tauri app
pub fn init_system_audio_state() -> SystemAudioDetectorState {
    Arc::new(Mutex::new(None))
}

// Event payload types for frontend
#[derive(serde::Serialize, Clone)]
pub struct SystemAudioStartedPayload {
    pub apps: Vec<String>,
}

#[derive(serde::Serialize, Clone)]
pub struct SystemAudioStoppedPayload;
