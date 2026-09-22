use anyhow::Result;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use crate::database::repositories::setting::SettingsRepository;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OnboardingStatus {
    pub version: String,
    pub completed: bool,
    pub current_step: u8,
    pub model_status: ModelStatus,
    pub last_updated: String,
    // The setup check (mic + system audio) is optional: older statuses never had one,
    // and a user who skips the check still completes onboarding without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_check: Option<SetupCheckRecord>,
}

/// Persisted outcome of the setup check (model load + microphone + system audio capture),
/// recorded so the app can re-offer the check from settings and so a user who already
/// proved their audio setup works is not asked to redo it every time they open the app.
///
/// This record has no serde rename attribute, matching every other field on
/// `OnboardingStatus`: its keys stay snake_case on the wire, unlike the camelCase
/// `SetupCheckResult` the `mic_check_start` command returns.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SetupCheckRecord {
    pub status: String,       // "passed" | "issues" | "skipped"
    pub model: String,        // the ModelReport outcome, verbatim
    pub microphone: String,   // the microphone ChannelOutcome, verbatim
    pub system_audio: String, // the system audio ChannelOutcome, verbatim
    pub checked_at: String,   // RFC3339
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ModelStatus {
    pub parakeet: String, // "downloaded" | "not_downloaded" | "downloading"
    pub summary: String,  // Generic field for summary model (Qwen 3.5 or legacy Gemma variants)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_summary_model: Option<String>,
}

impl Default for OnboardingStatus {
    fn default() -> Self {
        Self {
            version: "1.0".to_string(),
            completed: false,
            current_step: 1,
            model_status: ModelStatus {
                parakeet: "not_downloaded".to_string(),
                summary: "not_downloaded".to_string(), // Changed from gemma
                selected_summary_model: None,
            },
            last_updated: chrono::Utc::now().to_rfc3339(),
            setup_check: None,
        }
    }
}

/// Load onboarding status from store
pub async fn load_onboarding_status<R: Runtime>(app: &AppHandle<R>) -> Result<OnboardingStatus> {
    // Try to load from Tauri store
    let store = match app.store("onboarding-status.json") {
        Ok(store) => store,
        Err(e) => {
            warn!("Failed to access onboarding store: {}, using defaults", e);
            return Ok(OnboardingStatus::default());
        }
    };

    // Try to get the status from store
    let status = if let Some(value) = store.get("status") {
        match serde_json::from_value::<OnboardingStatus>(value.clone()) {
            Ok(s) => {
                info!(
                    "Loaded onboarding status from store - Step: {}, Completed: {}",
                    s.current_step, s.completed
                );
                s
            }
            Err(e) => {
                warn!(
                    "Failed to deserialize onboarding status: {}, using defaults",
                    e
                );
                OnboardingStatus::default()
            }
        }
    } else {
        info!("No stored onboarding status found, using defaults");
        OnboardingStatus::default()
    };

    Ok(status)
}

/// Save onboarding status to store
pub async fn save_onboarding_status<R: Runtime>(
    app: &AppHandle<R>,
    status: &OnboardingStatus,
) -> Result<()> {
    info!(
        "Saving onboarding status: step={}, completed={}",
        status.current_step, status.completed
    );

    // Get or create store
    let store = app
        .store("onboarding-status.json")
        .map_err(|e| anyhow::anyhow!("Failed to access onboarding store: {}", e))?;

    // Update last_updated timestamp
    let mut status = status.clone();
    status.last_updated = chrono::Utc::now().to_rfc3339();

    // The frontend's debounced auto-save rebuilds the whole OnboardingStatus from React
    // state on every save, and that React state does not always carry the setup check
    // result forward. If we saved `status` as given, a save that happens to omit
    // setup_check would silently erase a result the user already earned by running the
    // check. So: only overwrite the stored setup_check when this call actually supplies
    // one; otherwise keep whatever is already on disk.
    if status.setup_check.is_none() {
        if let Some(existing_value) = store.get("status") {
            if let Ok(existing) = serde_json::from_value::<OnboardingStatus>(existing_value) {
                status.setup_check = existing.setup_check;
            }
        }
    }

    // Serialize status to JSON value
    let status_value = serde_json::to_value(&status)
        .map_err(|e| anyhow::anyhow!("Failed to serialize onboarding status: {}", e))?;

    // Save to store
    store.set("status", status_value);

    // Persist to disk
    store
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save onboarding store to disk: {}", e))?;

    info!("Successfully persisted onboarding status to disk");
    Ok(())
}

/// Reset onboarding status (delete from store)
pub async fn reset_onboarding_status<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    info!("Resetting onboarding status");

    let store = app
        .store("onboarding-status.json")
        .map_err(|e| anyhow::anyhow!("Failed to access onboarding store: {}", e))?;

    // Clear the status key
    store.delete("status");

    // Persist deletion to disk
    store
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save onboarding store after reset: {}", e))?;

    info!("Successfully reset onboarding status");
    Ok(())
}

/// Tauri commands for onboarding status
#[tauri::command]
pub async fn get_onboarding_status<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<OnboardingStatus>, String> {
    let status = load_onboarding_status(&app)
        .await
        .map_err(|e| format!("Failed to load onboarding status: {}", e))?;

    // Return None if it's the default (never saved before)
    // Check if we have any saved data by seeing if the store has the key
    let store = app
        .store("onboarding-status.json")
        .map_err(|e| format!("Failed to access store: {}", e))?;

    if store.get("status").is_none() {
        Ok(None)
    } else {
        Ok(Some(status))
    }
}

#[tauri::command]
pub async fn save_onboarding_status_cmd<R: Runtime>(
    app: AppHandle<R>,
    status: OnboardingStatus,
) -> Result<(), String> {
    save_onboarding_status(&app, &status)
        .await
        .map_err(|e| format!("Failed to save onboarding status: {}", e))
}

#[tauri::command]
pub async fn reset_onboarding_status_cmd<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    reset_onboarding_status(&app)
        .await
        .map_err(|e| format!("Failed to reset onboarding status: {}", e))
}

#[tauri::command]
pub async fn complete_onboarding<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    model: String,
    setup_check: Option<SetupCheckRecord>,
) -> Result<(), String> {
    info!("Completing onboarding with builtin-ai model: {}", model);

    // Step 1: Save model configuration to SQLite database FIRST
    let pool = state.db_manager.pool();

    // Onboarding always uses builtin-ai (local LLM)
    if let Err(e) =
        SettingsRepository::save_model_config(pool, "builtin-ai", &model, "large-v3", None).await
    {
        error!("Failed to save builtin-ai model config: {}", e);
        return Err(format!("Failed to save builtin-ai model config: {}", e));
    }
    info!("Saved builtin-ai model config: model={}", model);

    // Save transcription model config (parakeet provider) - always parakeet
    if let Err(e) = SettingsRepository::save_transcript_config(
        pool,
        "parakeet",
        crate::config::DEFAULT_PARAKEET_MODEL,
    )
    .await
    {
        error!("Failed to save transcription model config: {}", e);
        return Err(format!("Failed to save transcription model config: {}", e));
    }
    info!(
        "Saved transcription model config: provider=parakeet, model={}",
        crate::config::DEFAULT_PARAKEET_MODEL
    );

    // Step 2: Only NOW mark onboarding as complete (after DB operations succeed)
    let mut status = load_onboarding_status(&app)
        .await
        .map_err(|e| format!("Failed to load onboarding status: {}", e))?;

    status.completed = true;
    status.current_step = 4; // Max step (4 on macOS with permissions, 3 on other platforms)
    status.model_status.parakeet = "downloaded".to_string();
    status.model_status.summary = "downloaded".to_string();
    status.model_status.selected_summary_model = Some(model.clone());
    // A user finishing setup without having run the check must not erase an earlier
    // result, so we only touch setup_check when the caller actually supplies one; the
    // status we just loaded already carries forward whatever was stored before.
    if let Some(setup_check) = setup_check {
        status.setup_check = Some(setup_check);
    }

    save_onboarding_status(&app, &status)
        .await
        .map_err(|e| format!("Failed to save completed onboarding status: {}", e))?;

    info!("Onboarding completed successfully with model: {}", model);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_status_deserializes_without_selected_summary_model() {
        let status: OnboardingStatus = serde_json::from_str(
            r#"{
                "version": "1.0",
                "completed": true,
                "current_step": 4,
                "model_status": {
                    "parakeet": "downloaded",
                    "summary": "downloaded"
                },
                "last_updated": "2026-05-30T00:00:00Z"
            }"#,
        )
        .expect("old onboarding status should remain compatible");

        assert_eq!(status.model_status.selected_summary_model, None);
        assert_eq!(status.setup_check.is_none(), true);
    }

    #[test]
    fn setup_check_record_round_trips_with_snake_case_keys() {
        let record = SetupCheckRecord {
            status: "issues".to_string(),
            model: "loaded".to_string(),
            microphone: "transcribed".to_string(),
            system_audio: "no_audio_detected".to_string(),
            checked_at: "2026-05-30T00:00:00Z".to_string(),
        };

        let value = serde_json::to_value(&record).expect("SetupCheckRecord should serialize");
        let object = value
            .as_object()
            .expect("SetupCheckRecord serializes to an object");

        assert_eq!(
            object.get("status").and_then(|v| v.as_str()),
            Some("issues")
        );
        assert_eq!(object.get("model").and_then(|v| v.as_str()), Some("loaded"));
        assert_eq!(
            object.get("microphone").and_then(|v| v.as_str()),
            Some("transcribed")
        );
        assert_eq!(
            object.get("system_audio").and_then(|v| v.as_str()),
            Some("no_audio_detected")
        );
        assert_eq!(
            object.get("checked_at").and_then(|v| v.as_str()),
            Some("2026-05-30T00:00:00Z")
        );
        // system_audio must stay snake_case: this is the key that would slip past
        // review, since everything else in this feature is camelCase on the wire.
        assert!(!object.contains_key("systemAudio"));

        let round_tripped: SetupCheckRecord =
            serde_json::from_value(value).expect("SetupCheckRecord should round-trip");
        assert_eq!(round_tripped.status, record.status);
        assert_eq!(round_tripped.model, record.model);
        assert_eq!(round_tripped.microphone, record.microphone);
        assert_eq!(round_tripped.system_audio, record.system_audio);
        assert_eq!(round_tripped.checked_at, record.checked_at);
    }

    #[test]
    fn onboarding_status_serializes_a_present_setup_check() {
        let mut status = OnboardingStatus::default();
        status.setup_check = Some(SetupCheckRecord {
            status: "passed".to_string(),
            model: "loaded".to_string(),
            microphone: "transcribed".to_string(),
            system_audio: "no_audio_detected".to_string(),
            checked_at: "2026-05-30T00:00:00Z".to_string(),
        });

        let value = serde_json::to_value(&status).expect("OnboardingStatus should serialize");
        let setup_check = value
            .get("setup_check")
            .expect("setup_check key should be present when Some")
            .as_object()
            .expect("setup_check serializes to an object");

        assert_eq!(
            setup_check.get("status").and_then(|v| v.as_str()),
            Some("passed")
        );
        assert_eq!(
            setup_check.get("system_audio").and_then(|v| v.as_str()),
            Some("no_audio_detected")
        );
    }
}
