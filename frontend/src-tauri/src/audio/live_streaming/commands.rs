//! Tauri commands for the live/final transcription settings. The live model
//! itself is downloaded with the other speech models (`audio/speech/`).

use log::info;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

use super::engine;
use crate::audio::speech::models::{self, ModelKind};
use crate::audio::transcription_settings::{
    self as settings, LiveEngineChoice, LiveTranscriptionSettings, AUTO_FINAL_PASS_KEY,
    LIVE_ENGINE_KEY,
};

const STORE_FILE: &str = "store.json";

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
) -> Result<LiveTranscriptionSettings, String> {
    Ok(settings::load_settings(&app))
}

#[tauri::command]
pub async fn set_live_transcription_settings<R: Runtime>(
    app: AppHandle<R>,
    auto_final_pass: Option<bool>,
    live_engine: Option<LiveEngineChoice>,
) -> Result<LiveTranscriptionSettings, String> {
    if let Some(choice) = live_engine {
        let previous = settings::load_settings(&app).live_engine;
        let value = serde_json::to_value(choice).map_err(|e| e.to_string())?;
        set_store_value(&app, LIVE_ENGINE_KEY, value.clone())?;
        info!("Live transcription engine set to {value}");
        // Auto and Neural Engine run the same engine: nothing to reload.
        if engine::compute_for(choice).arg() != engine::compute_for(previous).arg() {
            apply_engine_change(&app).await;
        }
    }
    if let Some(enabled) = auto_final_pass {
        set_store_value(&app, AUTO_FINAL_PASS_KEY, serde_json::json!(enabled))?;
        info!("Automatic final transcript pass set to {enabled}");
    }
    get_live_transcription_settings(app).await
}

/// After the engine setting changed: drop the loaded engine unless a
/// recording uses it (the next recording loads the new one), and compile the
/// new engine's models in the background if the live model is downloaded.
async fn apply_engine_change<R: Runtime>(app: &AppHandle<R>) {
    if !crate::audio::recording_commands::is_recording().await {
        engine::unload();
    }
    if models::is_downloaded(app, ModelKind::LiveAsr) {
        engine::spawn_prepare(app.clone(), false);
    }
}
