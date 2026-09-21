//! Debug mode: record test meetings into an isolated "debug" bucket so they do
//! not pollute real recordings, and expose diagnostics for troubleshooting.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

pub fn is_enabled() -> bool {
    DEBUG_MODE.load(Ordering::SeqCst)
}

fn apply_log_level(enabled: bool) {
    log::set_max_level(if enabled {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
}

/// Load the persisted preference at startup.
pub fn load<R: Runtime>(app: &AppHandle<R>) {
    let enabled = app
        .store("store.json")
        .ok()
        .and_then(|store| store.get("debugMode"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    DEBUG_MODE.store(enabled, Ordering::SeqCst);
    apply_log_level(enabled);
}

pub fn set<R: Runtime>(app: &AppHandle<R>, enabled: bool) {
    DEBUG_MODE.store(enabled, Ordering::SeqCst);
    apply_log_level(enabled);

    if let Ok(store) = app.store("store.json") {
        store.set("debugMode", serde_json::Value::Bool(enabled));
        let _ = store.save();
    }
    log::info!("Debug mode set to {}", enabled);
}

#[tauri::command]
pub fn get_debug_mode() -> bool {
    is_enabled()
}

#[tauri::command]
pub fn set_debug_mode<R: Runtime>(app: AppHandle<R>, enabled: bool) {
    set(&app, enabled);
}

/// A point-in-time snapshot for the Settings diagnostics panel.
#[tauri::command]
pub async fn get_debug_info<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<serde_json::Value, String> {
    let pool = state.db_manager.pool();

    let meetings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let debug_meetings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM meetings WHERE is_debug = 1")
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    let transcripts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transcripts")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let recordings_dir = crate::audio::recording_preferences::load_recording_preferences(&app)
        .await
        .map(|prefs| prefs.save_folder.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "appDataDir": app_data_dir,
        "recordingsDir": recordings_dir,
        "meetings": meetings,
        "debugMeetings": debug_meetings,
        "transcripts": transcripts,
    }))
}

/// Delete every debug meeting (cascades to transcripts/summaries/notes).
#[tauri::command]
pub async fn delete_debug_meetings<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<u64, String> {
    crate::database::repositories::meeting::MeetingsRepository::delete_debug_meetings(
        state.db_manager.pool(),
    )
    .await
    .map_err(|e| e.to_string())
}
