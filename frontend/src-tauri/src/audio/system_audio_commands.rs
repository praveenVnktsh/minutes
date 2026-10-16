use crate::audio::{
    check_system_audio_permissions, list_system_audio_devices, new_system_audio_callback,
    start_system_audio_capture, SystemAudioDetector, SystemAudioEvent,
};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tauri::{command, AppHandle, Emitter, State};

// Global state for system audio detector
type SystemAudioDetectorState = Arc<Mutex<Option<SystemAudioDetector>>>;

/// Start system audio capture (for capturing system output audio)
#[command]
pub async fn start_system_audio_capture_command() -> Result<String, String> {
    match start_system_audio_capture().await {
        Ok(_stream) => {
            // TODO: Store the stream in global state if needed for management
            Ok("System audio capture started successfully".to_string())
        }
        Err(e) => Err(format!("Failed to start system audio capture: {}", e)),
    }
}

/// List available system audio devices
#[command]
pub async fn list_system_audio_devices_command() -> Result<Vec<String>, String> {
    list_system_audio_devices().map_err(|e| format!("Failed to list system audio devices: {}", e))
}

/// Check if the app has permission to access system audio
#[command]
pub async fn check_system_audio_permissions_command() -> bool {
    check_system_audio_permissions()
}

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

/// Stop monitoring system audio usage
#[command]
pub async fn stop_system_audio_monitoring(
    detector_state: State<'_, SystemAudioDetectorState>,
) -> Result<(), String> {
    let mut detector_guard = detector_state
        .lock()
        .map_err(|e| format!("Failed to acquire detector lock: {}", e))?;

    if let Some(mut detector) = detector_guard.take() {
        detector.stop();
        Ok(())
    } else {
        Err("System audio monitoring is not active".to_string())
    }
}

/// Get the current status of system audio monitoring
#[command]
pub async fn get_system_audio_monitoring_status(
    detector_state: State<'_, SystemAudioDetectorState>,
) -> Result<bool, String> {
    let detector_guard = detector_state
        .lock()
        .map_err(|e| format!("Failed to acquire detector lock: {}", e))?;

    Ok(detector_guard.is_some())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_system_audio_devices() {
        let devices = list_system_audio_devices_command().await;
        match devices {
            Ok(device_list) => {
                // Smoke test: listing devices should not panic.
                println!("System audio devices: {:?}", device_list);
            }
            Err(e) => {
                println!("Error listing devices: {}", e);
                // This might fail on CI or systems without audio
            }
        }
    }

    #[tokio::test]
    async fn test_check_permissions() {
        let has_permission = check_system_audio_permissions_command().await;
        println!("Has system audio permissions: {}", has_permission);
        // This is mainly a smoke test to ensure it doesn't crash
    }
}
