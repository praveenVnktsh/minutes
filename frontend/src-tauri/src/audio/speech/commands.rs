//! Tauri commands for the speech model store: status, download, cancel,
//! delete, and opening the folder.

use log::info;
use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use super::models::{self, ModelKind, AUTO_DOWNLOAD_DISABLED_KEY};

const STORE_FILE: &str = "store.json";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: &'static str,
    pub kind: ModelKind,
    pub display_name: &'static str,
    pub size_bytes: u64,
    pub downloaded: bool,
    pub downloading: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelsStatus {
    /// False below macOS 15: transcription is unavailable and every `*Ready`
    /// is false, downloaded models or not.
    pub os_supported: bool,
    pub models: Vec<ModelStatus>,
    pub live_ready: bool,
    pub batch_ready: bool,
    pub diarization_ready: bool,
}

pub fn status<R: Runtime>(app: &AppHandle<R>) -> SpeechModelsStatus {
    let models = models::BUNDLES
        .iter()
        .map(|bundle| ModelStatus {
            id: bundle.id,
            kind: bundle.kind,
            display_name: bundle.display_name,
            size_bytes: bundle.approx_bytes,
            downloaded: models::is_downloaded(app, bundle.kind),
            downloading: models::is_downloading_kind(bundle.kind),
        })
        .collect();
    SpeechModelsStatus {
        os_supported: super::helper::os_supported(),
        models,
        live_ready: models::live_ready(app),
        batch_ready: models::batch_ready(app),
        diarization_ready: models::diarization_ready(app),
    }
}

fn set_auto_download_disabled<R: Runtime>(app: &AppHandle<R>, disabled: bool) {
    let saved = app
        .store(STORE_FILE)
        .map_err(|e| e.to_string())
        .and_then(|store| {
            store.set(AUTO_DOWNLOAD_DISABLED_KEY, serde_json::json!(disabled));
            store.save().map_err(|e| e.to_string())
        });
    if let Err(error) = saved {
        log::warn!("Could not save {AUTO_DOWNLOAD_DISABLED_KEY}: {error}");
    }
}

#[tauri::command]
pub async fn speech_models_status<R: Runtime>(app: AppHandle<R>) -> SpeechModelsStatus {
    status(&app)
}

/// Download `kinds`, or every missing model when `None`, and wait for it.
#[tauri::command]
pub async fn speech_models_download<R: Runtime>(
    app: AppHandle<R>,
    kinds: Option<Vec<ModelKind>>,
) -> Result<(), String> {
    set_auto_download_disabled(&app, false);
    let kinds = kinds.unwrap_or_else(|| ModelKind::ALL.to_vec());
    models::download(&app, &kinds)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn speech_models_cancel_download<R: Runtime>(app: AppHandle<R>) -> bool {
    let cancelled = models::cancel_download();
    if cancelled {
        // Not restarted on the next launch; an explicit download clears it.
        set_auto_download_disabled(&app, true);
    }
    cancelled
}

/// Delete a model, first stopping whatever has it loaded: the live engine
/// (refused while a recording uses it) or the batch helper.
#[tauri::command]
pub async fn speech_models_delete<R: Runtime>(
    app: AppHandle<R>,
    kind: ModelKind,
) -> Result<(), String> {
    match kind {
        ModelKind::LiveAsr => {
            if crate::audio::live_streaming::engine::is_loaded() {
                if crate::audio::recording_commands::is_recording().await {
                    return Err("The live model is in use by the current recording".to_string());
                }
                crate::audio::live_streaming::engine::unload();
            }
        }
        ModelKind::FinalAsr | ModelKind::Vad | ModelKind::Diarization => {
            // Fail fast rather than wait out a transcription or a diarization
            // (minutes) for the helper to come free.
            let busy = crate::audio::transcription_queue::get_queue()
                .has_active_task()
                .await
                || !super::batch::try_unload().await;
            if busy {
                return Err(format!(
                    "{} is in use by a transcription or speaker identification. Try again when it finishes.",
                    models::bundle(kind).display_name
                ));
            }
        }
    }
    models::delete(&app, kind).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn speech_models_open_folder<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let folder = models::dir(&app).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&folder).map_err(|e| format!("Failed to create the folder: {e}"))?;
    std::process::Command::new("open")
        .arg(&folder)
        .spawn()
        .map_err(|e| format!("Failed to open the folder: {e}"))?;
    info!("Opened speech models folder: {}", folder.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_camel_case_on_the_wire() {
        let status = SpeechModelsStatus {
            os_supported: true,
            models: vec![ModelStatus {
                id: "silero-vad-coreml",
                kind: ModelKind::Vad,
                display_name: "Silero VAD",
                size_bytes: 1_063_427,
                downloaded: true,
                downloading: false,
            }],
            live_ready: false,
            batch_ready: true,
            diarization_ready: false,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "osSupported": true,
                "models": [{
                    "id": "silero-vad-coreml",
                    "kind": "vad",
                    "displayName": "Silero VAD",
                    "sizeBytes": 1_063_427,
                    "downloaded": true,
                    "downloading": false,
                }],
                "liveReady": false,
                "batchReady": true,
                "diarizationReady": false,
            })
        );
    }

    #[test]
    fn download_kinds_arrive_in_kebab_case() {
        let kinds: Vec<ModelKind> = serde_json::from_value(serde_json::json!([
            "live-asr",
            "final-asr",
            "vad",
            "diarization"
        ]))
        .unwrap();
        assert_eq!(
            kinds,
            vec![
                ModelKind::LiveAsr,
                ModelKind::FinalAsr,
                ModelKind::Vad,
                ModelKind::Diarization
            ]
        );
    }
}
