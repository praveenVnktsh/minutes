//! Tauri commands for the live/final transcription settings and the live
//! model download, and the startup routine that applies the defaults.

use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use super::model;
use crate::audio::transcription_settings::{
    self as settings, LiveModelChoice, LiveTranscriptionSettings, AUTO_FINAL_PASS_KEY,
    LIVE_MODEL_KEY,
};

const STORE_FILE: &str = "store.json";
/// Set when the user cancels the live model download, so it is not restarted
/// automatically on every launch. Cleared by an explicit download.
const LIVE_AUTO_DOWNLOAD_DISABLED_KEY: &str = "liveModelAutoDownloadDisabled";
/// The default final model is fetched automatically once.
const FINAL_AUTO_DOWNLOAD_ATTEMPTED_KEY: &str = "finalModelAutoDownloadAttempted";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveModelStatus {
    pub model_id: &'static str,
    pub downloaded: bool,
    pub downloading: bool,
    pub loaded: bool,
    pub size_bytes: u64,
    /// False on platforms without the streaming runtime (everything but
    /// macOS): the settings show Nemotron as unavailable there.
    pub supported: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscriptionSettingsResponse {
    #[serde(flatten)]
    pub settings: LiveTranscriptionSettings,
    pub live_model_status: LiveModelStatus,
}

pub fn live_model_status<R: Runtime>(app: &AppHandle<R>) -> LiveModelStatus {
    LiveModelStatus {
        model_id: model::MODEL_ID,
        downloaded: model::is_downloaded(app),
        downloading: model::is_downloading(),
        loaded: super::engine::is_loaded(),
        size_bytes: model::total_bytes(),
        supported: super::SUPPORTED,
    }
}

fn set_store_value<R: Runtime>(
    app: &AppHandle<R>,
    key: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(key, value);
    store.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_live_transcription_settings<R: Runtime>(
    app: AppHandle<R>,
) -> Result<LiveTranscriptionSettingsResponse, String> {
    Ok(LiveTranscriptionSettingsResponse {
        settings: settings::load_settings(&app),
        live_model_status: live_model_status(&app),
    })
}

#[tauri::command]
pub async fn set_live_transcription_settings<R: Runtime>(
    app: AppHandle<R>,
    live_model: Option<String>,
    auto_final_pass: Option<bool>,
) -> Result<LiveTranscriptionSettingsResponse, String> {
    if let Some(live_model) = live_model {
        let choice = match live_model.as_str() {
            "nemotron-streaming" if !super::SUPPORTED => {
                return Err("Nemotron live transcription is only available on macOS".to_string())
            }
            "nemotron-streaming" => LiveModelChoice::NemotronStreaming,
            "chunked" => LiveModelChoice::Chunked,
            other => return Err(format!("Unknown live model '{other}'")),
        };
        set_store_value(&app, LIVE_MODEL_KEY, serde_json::json!(choice.as_str()))?;
        info!("Live transcription model set to {}", choice.as_str());
    }
    if let Some(enabled) = auto_final_pass {
        set_store_value(&app, AUTO_FINAL_PASS_KEY, serde_json::json!(enabled))?;
        info!("Automatic final transcript pass set to {enabled}");
    }
    get_live_transcription_settings(app).await
}

#[tauri::command]
pub async fn live_model_get_status<R: Runtime>(app: AppHandle<R>) -> LiveModelStatus {
    live_model_status(&app)
}

#[tauri::command]
pub async fn live_model_download<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if !super::SUPPORTED {
        return Err("Nemotron live transcription is only available on macOS".to_string());
    }
    let _ = set_store_value(
        &app,
        LIVE_AUTO_DOWNLOAD_DISABLED_KEY,
        serde_json::json!(false),
    );
    model::download(&app).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn live_model_cancel_download<R: Runtime>(app: AppHandle<R>) -> Result<bool, String> {
    let cancelled = model::cancel_download();
    if cancelled {
        let _ = set_store_value(
            &app,
            LIVE_AUTO_DOWNLOAD_DISABLED_KEY,
            serde_json::json!(true),
        );
    }
    Ok(cancelled)
}

#[tauri::command]
pub async fn live_model_delete<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if crate::audio::recording_commands::is_recording().await && super::engine::is_loaded() {
        return Err("The live model is in use by the current recording".to_string());
    }
    model::delete(&app).map_err(|e| e.to_string())
}

/// Apply the transcription defaults once onboarding is done: migrate the final
/// model, then fetch missing default models in the background. Downloads
/// report progress through their usual events, so the download toast and
/// settings show them. Failures are logged and retried on a later start.
pub fn spawn_default_model_setup(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        match crate::onboarding::load_onboarding_status(&app).await {
            Ok(status) if status.completed => {}
            Ok(_) => return, // Onboarding calls this again when it finishes.
            Err(error) => {
                warn!("Default transcription model setup skipped: {error}");
                return;
            }
        }

        settings::migrate_settings(&app).await;

        let store = app.store(STORE_FILE).ok();
        let flag = |key: &str| {
            store
                .as_ref()
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };

        let current = settings::load_settings(&app);
        if current.live_model == LiveModelChoice::NemotronStreaming
            && !model::is_downloaded(&app)
            && !model::is_downloading()
            && !flag(LIVE_AUTO_DOWNLOAD_DISABLED_KEY)
        {
            info!("Downloading the default live model (Nemotron streaming) in the background");
            if let Err(error) = model::download(&app).await {
                warn!("Background live model download did not finish: {error:#}");
            }
        }

        let final_model = settings::configured_final_model(&app).await;
        if final_model == settings::EngineModel::default_final()
            && !flag(FINAL_AUTO_DOWNLOAD_ATTEMPTED_KEY)
        {
            let available = settings::availability().await;
            if !available.whisper.iter().any(|m| m == &final_model.model) {
                info!(
                    "Downloading the default final transcript model ({}) in the background",
                    final_model.model
                );
                let result = crate::whisper_engine::commands::whisper_download_model(
                    app.clone(),
                    final_model.model.clone(),
                )
                .await;
                if let Err(error) = &result {
                    warn!("Background final model download did not finish, retrying on a later start: {error}");
                }
                if final_download_settled(&result) {
                    let _ = set_store_value(
                        &app,
                        FINAL_AUTO_DOWNLOAD_ATTEMPTED_KEY,
                        serde_json::json!(true),
                    );
                }
            }
        }
    });
}

/// Whether the automatic final-model download is done with for good:
/// `whisper_download_model` returns `Ok` when the model arrived or the user
/// cancelled it (cancellation is not an error there), and `Err` for a failure
/// such as a dropped connection, which is retried on a later start.
fn final_download_settled<E>(result: &Result<(), E>) -> bool {
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_final_download_is_retried_later() {
        assert!(!final_download_settled(&Err::<(), _>("connection reset")));
        // Finished, or cancelled by the user: do not start it again.
        assert!(final_download_settled(&Ok::<(), &str>(())));
    }
}
