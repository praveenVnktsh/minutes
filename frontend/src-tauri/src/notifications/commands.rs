use crate::notifications::{manager::NotificationManager, settings::NotificationSettings};

use anyhow::Result;
use log::{error as log_error, info as log_info};
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State, Wry};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::RwLock;

/// Shared notification manager state
pub type NotificationManagerState<R> = Arc<RwLock<Option<NotificationManager<R>>>>;

/// Initialize the notification manager (called during app setup)
pub async fn initialize_notification_manager<R: Runtime>(
    app_handle: AppHandle<R>,
) -> Result<NotificationManager<R>> {
    log_info!("Initializing notification manager...");

    let manager = NotificationManager::new(app_handle).await?;
    manager.initialize().await?;

    log_info!("Notification manager initialized successfully");
    Ok(manager)
}

/// Get notification settings
#[tauri::command]
pub async fn get_notification_settings(
    manager_state: State<'_, NotificationManagerState<Wry>>,
) -> Result<NotificationSettings, String> {
    log_info!("Getting notification settings");

    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        Ok(manager.get_settings().await)
    } else {
        Err("Notification manager not initialized".to_string())
    }
}

/// Set notification settings
#[tauri::command]
pub async fn set_notification_settings(
    settings: NotificationSettings,
    manager_state: State<'_, NotificationManagerState<Wry>>,
) -> Result<(), String> {
    log_info!("Setting notification settings");

    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        manager
            .update_settings(settings)
            .await
            .map_err(|e| format!("Failed to update settings: {}", e))
    } else {
        Err("Notification manager not initialized".to_string())
    }
}

// Helper functions for showing specific notification types
// These are used internally by the app and don't need to be Tauri commands

/// Show recording started notification (internal use)
pub async fn show_recording_started_notification<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    manager_state: &NotificationManagerState<R>,
    meeting_name: Option<String>,
) -> Result<()> {
    log_info!(
        "Attempting to show recording started notification for meeting: {:?}",
        meeting_name
    );

    // Check if manager is initialized
    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        log_info!("Notification manager found, showing recording started notification");
        manager.show_recording_started(meeting_name).await
    } else {
        drop(manager_lock);
        log_info!("Notification manager not initialized, initializing now...");

        // Try to initialize the manager first
        match initialize_notification_manager(app_handle.clone()).await {
            Ok(manager) => {
                // Store the manager in the state
                let mut state_lock = manager_state.write().await;
                *state_lock = Some(manager);
                drop(state_lock);

                log_info!("Notification manager initialized, showing notification...");

                // Now use the initialized manager
                let manager_lock = manager_state.read().await;
                if let Some(manager) = manager_lock.as_ref() {
                    manager.show_recording_started(meeting_name).await
                } else {
                    log_error!("Manager still not available after initialization");
                    Ok(())
                }
            }
            Err(e) => {
                log_error!("Failed to initialize notification manager: {}", e);

                // Check settings before showing fallback notification
                use crate::notifications::settings::ConsentManager;
                let consent_manager = ConsentManager::new(app_handle.clone())?;
                let settings = consent_manager.load_settings().await.unwrap_or_default();

                if !settings.notification_preferences.show_recording_started {
                    log_info!(
                        "Recording started notification is disabled in settings, skipping fallback"
                    );
                    return Ok(());
                }

                // Fallback: Use Tauri's notification API directly
                let title = crate::APP_NAME;
                let body = match meeting_name {
                    Some(name) => format!("Recording started for meeting: {}", name),
                    None => "Recording has started. Please inform others in the meeting that you are recording.".to_string(),
                };

                log_info!(
                    "Using direct Tauri notification fallback: {} - {}",
                    title,
                    body
                );

                match app_handle
                    .notification()
                    .builder()
                    .title(title)
                    .body(body)
                    .show()
                {
                    Ok(_) => {
                        log_info!("Successfully showed fallback notification: {}", title);
                        Ok(())
                    }
                    Err(e) => {
                        log_error!("Failed to show fallback notification: {}", e);
                        Err(anyhow::anyhow!("Failed to show notification: {}", e))
                    }
                }
            }
        }
    }
}

/// Show recording stopped notification (internal use)
pub async fn show_recording_stopped_notification<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    manager_state: &NotificationManagerState<R>,
) -> Result<()> {
    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        manager.show_recording_stopped().await
    } else {
        drop(manager_lock);
        log_info!("Notification manager not initialized for stop notification, using fallback...");

        // Check settings before showing fallback notification
        use crate::notifications::settings::ConsentManager;
        let consent_manager = ConsentManager::new(app_handle.clone())?;
        let settings = consent_manager.load_settings().await.unwrap_or_default();

        if !settings.notification_preferences.show_recording_stopped {
            log_info!("Recording stopped notification is disabled in settings, skipping fallback");
            return Ok(());
        }

        // Use direct Tauri notification as fallback for stop notification
        let title = crate::APP_NAME;
        let body = "Recording has stopped";

        log_info!(
            "Using direct Tauri notification fallback: {} - {}",
            title,
            body
        );

        match app_handle
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
        {
            Ok(_) => {
                log_info!("Successfully showed fallback notification: {}", title);
                Ok(())
            }
            Err(e) => {
                log_error!("Failed to show fallback notification: {}", e);
                Err(anyhow::anyhow!("Failed to show notification: {}", e))
            }
        }
    }
}

/// Show recording paused notification (internal use)
pub async fn show_recording_paused_notification(
    manager_state: &NotificationManagerState<Wry>,
) -> Result<()> {
    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        manager.show_recording_paused().await
    } else {
        log_error!("Cannot show recording paused notification: manager not initialized");
        Ok(())
    }
}

/// Show recording resumed notification (internal use)
pub async fn show_recording_resumed_notification(
    manager_state: &NotificationManagerState<Wry>,
) -> Result<()> {
    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        manager.show_recording_resumed().await
    } else {
        log_error!("Cannot show recording resumed notification: manager not initialized");
        Ok(())
    }
}

/// Show transcription complete notification (internal use)
pub async fn show_transcription_complete_notification(
    manager_state: &NotificationManagerState<Wry>,
    file_path: Option<String>,
) -> Result<()> {
    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        manager.show_transcription_complete(file_path).await
    } else {
        log_error!("Cannot show transcription complete notification: manager not initialized");
        Ok(())
    }
}

/// Show system error notification (internal use)
pub async fn show_system_error_notification(
    manager_state: &NotificationManagerState<Wry>,
    error: String,
) -> Result<()> {
    let manager_lock = manager_state.read().await;
    if let Some(manager) = manager_lock.as_ref() {
        manager.show_system_error(error).await
    } else {
        log_error!("Cannot show system error notification: manager not initialized");
        Ok(())
    }
}
