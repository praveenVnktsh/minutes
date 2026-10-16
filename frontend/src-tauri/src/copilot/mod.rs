//! Live Copilot: answers about the last few minutes while a meeting records,
//! and suggested replies when someone else asks the user something.
//!
//! One `Core` per recording session, owned by the recording lifecycle like the
//! live summary. `start_session` / `finish_session` are infallible and return
//! at once; nothing here can fail or delay a recording.
//!
//! See docs/superpowers/specs/2026-10-16-live-copilot-design.md.

pub mod context;
pub mod core;
pub mod detector;
pub mod persist;
pub mod prompt;
pub mod stream;
pub mod stream_parse;

#[cfg(test)]
mod smoke;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, EventId, Listener, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use crate::database::repositories::setting::SettingsRepository;
use crate::meeting_activity::{self, ActivityStatus};
use crate::state::AppState;
use crate::summary::llm_client::LLMProvider;
use crate::summary::service::load_llm_config;
use crate::summary::summary_engine::models;
use context::{Line, Speaker, TimelineItem};
use core::{ChatRow, Core, CoreConfig, Delta, Host, Snapshot};
use prompt::QuickAction;
use stream::{ProviderStreamer, Streamer};

pub const STATE_EVENT: &str = "copilot-state";
pub const DELTA_EVENT: &str = "copilot-delta";
const STORE_FILE: &str = "store.json";
pub const SUGGESTIONS_KEY: &str = "copilotSuggestionsEnabled";
pub const DEBUG_KEY: &str = "copilotDebug";
pub const MODEL_KEY: &str = "copilotModel";
/// How long a finished session keeps trying to save moments while the
/// meeting id is still being bound.
const FINAL_PERSIST_WINDOW: Duration = Duration::from_secs(60);
const FINAL_PERSIST_RETRY: Duration = Duration::from_secs(2);

struct ActiveSession {
    core: Arc<Core>,
    listener: Option<EventId>,
}

static SESSION: Mutex<Option<ActiveSession>> = Mutex::new(None);

fn current_core() -> Option<Arc<Core>> {
    SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.core.clone())
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// A provider and model for Copilot, when it should not use the summary model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotModel {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSettings {
    pub suggestions_enabled: bool,
    pub debug: bool,
    /// `None`: use the summary model.
    pub model: Option<CopilotModel>,
}

/// Reads settings from stored values. Absent keys: suggestions on, debug
/// off, the summary model.
pub fn settings_from_stored(
    suggestions: Option<&serde_json::Value>,
    debug: Option<&serde_json::Value>,
    model: Option<&serde_json::Value>,
) -> CopilotSettings {
    CopilotSettings {
        suggestions_enabled: suggestions
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        debug: debug.and_then(serde_json::Value::as_bool).unwrap_or(false),
        model: model
            .and_then(|value| serde_json::from_value::<CopilotModel>(value.clone()).ok())
            .filter(|m| !m.provider.trim().is_empty() && !m.model.trim().is_empty()),
    }
}

pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> CopilotSettings {
    match app.store(STORE_FILE) {
        Ok(store) => settings_from_stored(
            store.get(SUGGESTIONS_KEY).as_ref(),
            store.get(DEBUG_KEY).as_ref(),
            store.get(MODEL_KEY).as_ref(),
        ),
        Err(error) => {
            warn!("Copilot settings unavailable, using defaults: {error}");
            settings_from_stored(None, None, None)
        }
    }
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

struct AppHost<R: Runtime> {
    app: AppHandle<R>,
    session_id: String,
}

impl<R: Runtime> AppHost<R> {
    fn pool(&self) -> Result<sqlx::SqlitePool, String> {
        self.app
            .try_state::<AppState>()
            .map(|state| state.db_manager.pool().clone())
            .ok_or_else(|| "The database is not ready".to_string())
    }

    fn meeting_id(&self) -> Option<String> {
        meeting_activity::recording_identity()
            .filter(|r| r.session_id == self.session_id)
            .and_then(|r| r.meeting_id)
            .or_else(|| meeting_activity::task_meeting_id(&self.session_id))
    }
}

/// The first built-in model that is downloaded, for "Use built-in model instead".
fn downloaded_builtin_model(app_data_dir: &std::path::Path) -> Option<String> {
    models::get_available_models()
        .into_iter()
        .map(|m| m.name)
        .find(|name| {
            models::get_model_path(app_data_dir, name)
                .map(|path| path.exists())
                .unwrap_or(false)
        })
}

#[async_trait]
impl<R: Runtime> Host for AppHost<R> {
    fn emit_state(&self, snapshot: &Snapshot) {
        if let Err(error) = self.app.emit(STATE_EVENT, snapshot) {
            warn!("Copilot: could not emit state: {error}");
        }
    }

    fn emit_delta(&self, delta: &Delta) {
        let _ = self.app.emit(DELTA_EVENT, delta);
    }

    fn timeline(&self) -> Vec<TimelineItem> {
        crate::live_summary::entries_for_session(&self.session_id)
            .into_iter()
            .map(|entry| TimelineItem {
                start_ms: entry.start_ms,
                kind: entry.kind,
                text: entry.text,
            })
            .collect()
    }

    fn suggestions_enabled(&self) -> bool {
        load_settings(&self.app).suggestions_enabled
    }

    fn debug_enabled(&self) -> bool {
        load_settings(&self.app).debug
    }

    fn is_paused(&self) -> bool {
        meeting_activity::recording_identity()
            .is_some_and(|r| r.session_id == self.session_id && r.status == ActivityStatus::Paused)
    }

    fn live_backlog_secs(&self) -> f64 {
        crate::audio::live_streaming::worker::current_backlog_seconds()
    }

    async fn streamer(&self, force_builtin: bool) -> Result<Arc<dyn Streamer>, String> {
        let pool = self.pool()?;
        let app_data_dir: Option<PathBuf> = self.app.path().app_data_dir().ok();
        let (provider, model) = if force_builtin {
            let dir = app_data_dir
                .as_deref()
                .ok_or("The app data folder is unavailable")?;
            let model = downloaded_builtin_model(dir)
                .ok_or("No built-in model is downloaded. Download one in Settings → Summary.")?;
            ("builtin-ai".to_string(), model)
        } else if let Some(choice) = load_settings(&self.app).model {
            (choice.provider, choice.model)
        } else {
            let settings = SettingsRepository::get_model_config(&pool)
                .await
                .map_err(|e| format!("Could not read the summary model setting: {e}"))?
                .filter(|s| !s.provider.trim().is_empty() && !s.model.trim().is_empty())
                .ok_or("Choose a summary model (or a Copilot model) in Settings first.")?;
            (settings.provider, settings.model)
        };
        let config = load_llm_config(&pool, &provider, &model, None).await?;
        let num_ctx = crate::summary::service::ollama_num_ctx(
            &config.provider,
            &config.model_name,
            config.ollama_endpoint.as_deref(),
        )
        .await;
        Ok(Arc::new(ProviderStreamer {
            provider_id: provider,
            config,
            app_data_dir,
            client: reqwest::Client::new(),
            num_ctx,
        }))
    }

    async fn persist(&self, rows: &[ChatRow]) -> Result<bool, String> {
        let Some(meeting_id) = self.meeting_id() else {
            return Ok(false);
        };
        let pool = self.pool()?;
        match persist::insert_rows(&pool, &meeting_id, rows).await {
            Ok(()) => Ok(true),
            Err(error) => {
                let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
                    .bind(&meeting_id)
                    .fetch_optional(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                if exists.is_none() {
                    // Deleted or discarded during the recording: drop them.
                    info!("Copilot: the meeting is gone, dropping {} rows", rows.len());
                    Ok(true)
                } else {
                    Err(error.to_string())
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// The parts of a `transcript-update` payload Copilot reads.
#[derive(Debug, Deserialize)]
struct TranscriptEvent {
    text: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    is_partial: bool,
    #[serde(default)]
    audio_start_time: f64,
    #[serde(default)]
    audio_end_time: f64,
}

fn line_from_event(payload: &str) -> Option<Line> {
    let event: TranscriptEvent = serde_json::from_str(payload).ok()?;
    if event.is_partial || event.text.trim().is_empty() {
        return None;
    }
    let ms = |s: f64| {
        if s.is_finite() && s > 0.0 {
            (s * 1000.0).round() as i64
        } else {
            0
        }
    };
    let start_ms = ms(event.audio_start_time);
    Some(Line {
        start_ms,
        end_ms: ms(event.audio_end_time).max(start_ms),
        speaker: Speaker::from_source(&event.source),
        text: event.text,
    })
}

/// Start Copilot for a recording session. Infallible and immediate; makes no
/// model request until a line or a command asks for one.
pub fn start_session<R: Runtime>(app: &AppHandle<R>, session_id: String) {
    let host: Arc<dyn Host> = Arc::new(AppHost {
        app: app.clone(),
        session_id: session_id.clone(),
    });
    let core = Core::new(session_id.clone(), host, CoreConfig::default());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Line>();
    let listener = app.listen("transcript-update", move |event: tauri::Event| {
        if let Some(line) = line_from_event(event.payload()) {
            let _ = sender.send(line);
        }
    });
    let previous = SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace(ActiveSession {
            core: core.clone(),
            listener: Some(listener),
        });
    if let Some(previous) = previous {
        warn!(
            "Copilot: finishing stale session {} before {session_id}",
            previous.core.session_id()
        );
        finish(app, previous);
    }
    tauri::async_runtime::spawn(async move {
        info!("Copilot ready for session {session_id}");
        while let Some(line) = receiver.recv().await {
            if !core.is_active() {
                break;
            }
            core.on_line(line);
        }
    });
}

fn finish<R: Runtime>(app: &AppHandle<R>, mut session: ActiveSession) {
    if let Some(listener) = session.listener.take() {
        app.unlisten(listener);
    }
    let core = session.core.clone();
    tauri::async_runtime::spawn(async move {
        core.finish(FINAL_PERSIST_WINDOW, FINAL_PERSIST_RETRY).await;
        info!("Copilot finished for session {}", core.session_id());
    });
}

/// The meeting ended: no more Copilot requests; in-flight ones are stopped
/// (inside the helper, never by killing it) and moments are saved. Returns at
/// once. The finished session stays readable for the panel until the next one.
pub fn finish_session<R: Runtime>(app: &AppHandle<R>, session_id: &str) {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        Some(session) if session.core.session_id() == session_id => {
            let finishing = ActiveSession {
                core: session.core.clone(),
                listener: session.listener.take(),
            };
            drop(guard);
            finish(app, finishing);
        }
        Some(session) => warn!(
            "Copilot: stop for {session_id} but session {} is active",
            session.core.session_id()
        ),
        None => {}
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn active_core() -> Result<Arc<Core>, String> {
    current_core()
        .filter(|core| core.is_active())
        .ok_or_else(|| "Copilot is only available while a meeting is recording".to_string())
}

#[tauri::command]
pub async fn copilot_get_state() -> Option<Snapshot> {
    current_core().map(|core| core.snapshot())
}

#[tauri::command]
pub async fn copilot_ask(
    action: Option<String>,
    question: Option<String>,
) -> Result<String, String> {
    let action = match action.as_deref() {
        Some(value) => Some(QuickAction::parse(value).ok_or("Unknown quick action")?),
        None => None,
    };
    active_core()?.ask(action, question)
}

#[tauri::command]
pub async fn copilot_stop(id: String) -> Result<(), String> {
    if let Some(core) = current_core() {
        core.stop(&id);
    }
    Ok(())
}

#[tauri::command]
pub async fn copilot_retry(id: String, use_builtin: bool) -> Result<(), String> {
    active_core()?.retry(&id, use_builtin)
}

#[tauri::command]
pub async fn copilot_shorter(id: String) -> Result<String, String> {
    active_core()?.shorter(&id)
}

#[tauri::command]
pub async fn copilot_dismiss_card(id: String, not_for_me: bool) -> Result<(), String> {
    if let Some(core) = current_core() {
        core.dismiss_card(&id, not_for_me);
    }
    Ok(())
}

#[tauri::command]
pub async fn copilot_set_notes(text: String) -> Result<(), String> {
    if let Some(core) = current_core().filter(|core| core.is_active()) {
        core.set_notes(text);
    }
    Ok(())
}

#[tauri::command]
pub async fn get_copilot_settings<R: Runtime>(app: AppHandle<R>) -> CopilotSettings {
    load_settings(&app)
}

#[tauri::command]
pub async fn set_copilot_settings<R: Runtime>(
    app: AppHandle<R>,
    settings: CopilotSettings,
) -> Result<CopilotSettings, String> {
    if let Some(model) = &settings.model {
        LLMProvider::from_str(&model.provider)?;
    }
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(
        SUGGESTIONS_KEY,
        serde_json::json!(settings.suggestions_enabled),
    );
    store.set(DEBUG_KEY, serde_json::json!(settings.debug));
    store.set(
        MODEL_KEY,
        serde_json::to_value(&settings.model).map_err(|e| e.to_string())?,
    );
    store.save().map_err(|e| e.to_string())?;
    // The panel shows the new switches at once.
    if let Some(core) = current_core() {
        app.emit(STATE_EVENT, core.snapshot()).ok();
    }
    Ok(load_settings(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_suggestions_on_debug_off_summary_model() {
        let defaults = settings_from_stored(None, None, None);
        assert!(defaults.suggestions_enabled);
        assert!(!defaults.debug);
        assert!(defaults.model.is_none());
        let custom = settings_from_stored(
            Some(&serde_json::json!(false)),
            Some(&serde_json::json!(true)),
            Some(&serde_json::json!({"provider": "groq", "model": "llama-3.3-70b"})),
        );
        assert!(!custom.suggestions_enabled);
        assert!(custom.debug);
        assert_eq!(custom.model.unwrap().provider, "groq");
        let blank = settings_from_stored(
            None,
            None,
            Some(&serde_json::json!({"provider": "", "model": ""})),
        );
        assert!(blank.model.is_none());
        assert!(
            settings_from_stored(None, None, Some(&serde_json::Value::Null))
                .model
                .is_none()
        );
    }

    #[test]
    fn final_lines_become_copilot_lines_and_partials_do_not() {
        let line = line_from_event(
            r#"{"text":"Can you send it?","source":"system","is_partial":false,"audio_start_time":61.5,"audio_end_time":63.0}"#,
        )
        .unwrap();
        assert_eq!(line.speaker, Speaker::Others);
        assert_eq!((line.start_ms, line.end_ms), (61_500, 63_000));
        assert!(line_from_event(
            r#"{"text":"Can","source":"system","is_partial":true,"audio_start_time":1,"audio_end_time":2}"#
        )
        .is_none());
        assert_eq!(
            line_from_event(
                r#"{"text":"Hi","source":"mic","audio_start_time":1,"audio_end_time":2}"#
            )
            .unwrap()
            .speaker,
            Speaker::You
        );
    }
}
