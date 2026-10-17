//! Live summary timeline: short points, decisions and actions made from the
//! live transcript while a meeting records, kept as the meeting's chapters.
//!
//! One worker per recording session, owned by the recording lifecycle (not by
//! any page). Nothing here can fail or delay a recording: `start_session` and
//! `finish_session` are infallible and return immediately, and every error
//! inside only changes the feature's own status.
//!
//! See docs/superpowers/specs/2026-10-16-live-summary-timeline-design.md.

pub mod facts;
pub mod glossary;
pub mod llm;
pub mod model_choice;
pub mod notes;
pub mod parse;
pub mod prompt;
pub mod scheduler;
pub mod worker;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod eval;
#[cfg(test)]
mod smoke;

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, EventId, Listener, Manager, Runtime, State};
use tauri_plugin_store::StoreExt;
use tokio_util::sync::CancellationToken;

use crate::database::repositories::timeline::{TimelineEntryRow, TimelineRepository};
use crate::meeting_activity::{self, ActivityStatus};
use crate::state::AppState;
use llm::{Generator, SummaryModelGenerator};
use scheduler::{Segment, Speaker};
use worker::{TimelineSnapshot, WorkerConfig, WorkerHost};

pub const TIMELINE_UPDATE_EVENT: &str = "live-timeline-update";
const STORE_FILE: &str = "store.json";
pub const ENABLED_KEY: &str = "liveSummaryEnabled";

struct ActiveSession {
    session_id: String,
    finish: CancellationToken,
}

static SESSION: Mutex<Option<ActiveSession>> = Mutex::new(None);
/// The latest snapshot of the current (or last) session, for the panel to
/// rehydrate after navigating.
static LATEST: Mutex<Option<TimelineSnapshot>> = Mutex::new(None);

/// Whether the live summary is on. An absent key means on.
pub fn enabled_from_stored(value: Option<&serde_json::Value>) -> bool {
    value.and_then(serde_json::Value::as_bool).unwrap_or(true)
}

pub fn is_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    match app.store(STORE_FILE) {
        Ok(store) => enabled_from_stored(store.get(ENABLED_KEY).as_ref()),
        Err(error) => {
            warn!("Live summary setting unavailable, using the default: {error}");
            true
        }
    }
}

/// The parts of a `transcript-update` payload the timeline reads.
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

fn segment_from_event(payload: &str, received_at: std::time::Duration) -> Option<Segment> {
    let event: TranscriptEvent = serde_json::from_str(payload).ok()?;
    if event.is_partial || event.text.trim().is_empty() {
        return None;
    }
    let seconds_to_ms = |s: f64| {
        if s.is_finite() && s > 0.0 {
            (s * 1000.0).round() as i64
        } else {
            0
        }
    };
    let start_ms = seconds_to_ms(event.audio_start_time);
    Some(Segment {
        start_ms,
        end_ms: seconds_to_ms(event.audio_end_time).max(start_ms),
        speaker: Speaker::from_source(&event.source),
        text: event.text,
        received_at,
    })
}

struct AppHost<R: Runtime> {
    app: AppHandle<R>,
    session_id: String,
}

impl<R: Runtime> AppHost<R> {
    fn pool(&self) -> Result<sqlx::SqlitePool> {
        self.app
            .try_state::<AppState>()
            .map(|state| state.db_manager.pool().clone())
            .ok_or_else(|| anyhow!("The database is not ready"))
    }
}

#[async_trait]
impl<R: Runtime> WorkerHost for AppHost<R> {
    fn is_paused(&self) -> bool {
        meeting_activity::recording_identity()
            .is_some_and(|r| r.session_id == self.session_id && r.status == ActivityStatus::Paused)
    }

    fn session_alive(&self) -> bool {
        meeting_activity::recording_identity()
            .is_some_and(|r| r.session_id == self.session_id && r.status != ActivityStatus::Failed)
    }

    fn meeting_id(&self) -> Option<String> {
        meeting_activity::recording_identity()
            .filter(|r| r.session_id == self.session_id)
            .and_then(|r| r.meeting_id)
            .or_else(|| meeting_activity::task_meeting_id(&self.session_id))
    }

    fn enabled(&self) -> bool {
        is_enabled(&self.app)
    }

    async fn generator(&self) -> Result<Arc<dyn Generator>> {
        let pool = self.pool()?;
        let generator = SummaryModelGenerator::from_settings(
            &pool,
            self.app.path().app_data_dir().ok(),
            model_choice::choice(&self.app),
        )
        .await?;
        Ok(Arc::new(generator))
    }

    async fn persist(&self, rows: &[TimelineEntryRow]) -> Result<()> {
        TimelineRepository::insert_entries(&self.pool()?, rows).await?;
        Ok(())
    }

    async fn meeting_exists(&self, meeting_id: &str) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&self.pool()?)
            .await?;
        Ok(row.is_some())
    }

    fn publish(&self, snapshot: &TimelineSnapshot) {
        let Some(snapshot) =
            publishable(&self.session_id, active_session_id().as_deref(), snapshot)
        else {
            return;
        };
        let mut latest = LATEST.lock().unwrap_or_else(|e| e.into_inner());
        if latest_accepts(latest.as_ref(), &snapshot) {
            *latest = Some(snapshot.clone());
        }
        drop(latest);
        if let Err(error) = self.app.emit(TIMELINE_UPDATE_EVENT, &snapshot) {
            warn!("Live summary: could not emit an update: {error}");
        }
    }
}

fn active_session_id() -> Option<String> {
    SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|session| session.session_id.clone())
}

/// What a session's worker may publish while `active` is the live session.
/// A worker still finishing after a newer recording started (it has up to
/// `STOP_GRACE` plus its last writes) only reports itself as stopped, so its
/// late updates can refresh the old meeting's chapters but never take over
/// the new session's live panel.
fn publishable(
    session_id: &str,
    active: Option<&str>,
    snapshot: &TimelineSnapshot,
) -> Option<TimelineSnapshot> {
    debug_assert_eq!(snapshot.session_id, session_id);
    match active {
        Some(active) if active != session_id => Some(TimelineSnapshot {
            status: worker::TimelineStatus::Stopped,
            error: None,
            ..snapshot.clone()
        }),
        _ => Some(snapshot.clone()),
    }
}

/// Whether `LATEST` (what the panel rehydrates from) takes `next`: the same
/// session's updates, or a live session replacing a stopped one; never an
/// older session's stopped snapshot over a newer one.
fn latest_accepts(current: Option<&TimelineSnapshot>, next: &TimelineSnapshot) -> bool {
    match current {
        None => true,
        Some(current) if current.session_id == next.session_id => true,
        Some(_) => next.status != worker::TimelineStatus::Stopped,
    }
}

/// Start the live summary for a recording session. Infallible and immediate.
pub fn start_session<R: Runtime>(app: &AppHandle<R>, session_id: String) {
    if let Some(previous) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take() {
        warn!(
            "Live summary: finishing stale session {} before {session_id}",
            previous.session_id
        );
        previous.finish.cancel();
    }
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = None;

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let started = crate::clock::now();
    let listener: EventId = app.listen("transcript-update", move |event: tauri::Event| {
        let received_at = crate::clock::now().saturating_duration_since(started);
        if let Some(segment) = segment_from_event(event.payload(), received_at) {
            let _ = sender.send(segment);
        }
    });

    let finish = CancellationToken::new();
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(ActiveSession {
        session_id: session_id.clone(),
        finish: finish.clone(),
    });

    let host: Arc<dyn WorkerHost> = Arc::new(AppHost {
        app: app.clone(),
        session_id: session_id.clone(),
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        info!("Live summary started for session {session_id}");
        let worker = tokio::spawn(worker::run(
            host,
            session_id.clone(),
            receiver,
            finish,
            WorkerConfig::default(),
        ));
        match worker.await {
            Ok(snapshot) => info!(
                "Live summary finished for session {session_id}: {} entries",
                snapshot.entries.len()
            ),
            Err(error) => warn!("Live summary worker for {session_id} ended abnormally: {error}"),
        }
        app.unlisten(listener);
        let mut active = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if active.as_ref().is_some_and(|s| s.session_id == session_id) {
            *active = None;
        }
    });
}

/// Tell the session's worker the recording stopped. It finishes its last
/// window within `worker::STOP_GRACE` on its own; this returns immediately.
pub fn finish_session(session_id: &str) {
    let active = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    match active.as_ref() {
        Some(session) if session.session_id == session_id => session.finish.cancel(),
        Some(session) => warn!(
            "Live summary: stop for {session_id} but session {} is active",
            session.session_id
        ),
        None => {}
    }
}

/// The live summary entries of recording session `session_id` so far, for
/// Copilot's context. Empty when the timeline belongs to another session.
pub fn entries_for_session(session_id: &str) -> Vec<worker::TimelineEntry> {
    LATEST
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|snapshot| snapshot.session_id == session_id)
        .map(|snapshot| snapshot.entries.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSummarySettings {
    pub enabled: bool,
}

/// The current (or last) session's timeline, so the panel can rehydrate.
#[tauri::command]
pub async fn get_live_timeline() -> Option<TimelineSnapshot> {
    LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[tauri::command]
pub async fn list_meeting_timeline_entries(
    meeting_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<TimelineEntryRow>, String> {
    TimelineRepository::list_for_meeting(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|e| format!("Could not load the meeting's chapters: {e}"))
}

#[tauri::command]
pub async fn get_live_summary_settings<R: Runtime>(app: AppHandle<R>) -> LiveSummarySettings {
    LiveSummarySettings {
        enabled: is_enabled(&app),
    }
}

#[tauri::command]
pub async fn set_live_summary_settings<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
) -> Result<LiveSummarySettings, String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(ENABLED_KEY, serde_json::json!(enabled));
    store.save().map_err(|e| e.to_string())?;
    Ok(LiveSummarySettings { enabled })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn snapshot(session: &str, status: worker::TimelineStatus) -> TimelineSnapshot {
        TimelineSnapshot {
            session_id: session.into(),
            meeting_id: Some(format!("meeting-{session}")),
            status,
            error: None,
            entries: Vec::new(),
        }
    }

    #[test]
    fn a_finishing_old_session_cannot_take_over_the_new_one() {
        use worker::TimelineStatus::*;
        // The old worker publishes "listening" after a new recording started.
        let late = publishable("old", Some("new"), &snapshot("old", Listening)).unwrap();
        assert_eq!(late.status, Stopped);
        assert_eq!(late.meeting_id.as_deref(), Some("meeting-old"));
        // ...and LATEST keeps the new session.
        let current = snapshot("new", Listening);
        assert!(!latest_accepts(Some(&current), &late));
        // Its own updates pass through untouched.
        let own = publishable("new", Some("new"), &current).unwrap();
        assert_eq!(own.status, Listening);
        // A new live session replaces a stopped one.
        assert!(latest_accepts(
            Some(&snapshot("old", Stopped)),
            &snapshot("new", Listening)
        ));
        // With no session active, a worker's own final update is kept.
        assert_eq!(
            publishable("old", None, &snapshot("old", Stopped))
                .unwrap()
                .status,
            Stopped
        );
    }

    #[test]
    fn the_setting_defaults_to_on() {
        assert!(enabled_from_stored(None));
        assert!(enabled_from_stored(Some(&serde_json::json!("junk"))));
        assert!(!enabled_from_stored(Some(&serde_json::json!(false))));
    }

    #[test]
    fn final_transcript_events_become_segments() {
        let payload = r#"{"text":"Ship it Friday","timestamp":"10:00:00","source":"system","sequence_id":4,"chunk_start_time":0,"is_partial":false,"confidence":0.9,"audio_start_time":61.25,"audio_end_time":63.5,"duration":2.25}"#;
        let segment = segment_from_event(payload, Duration::from_secs(3)).unwrap();
        assert_eq!(segment.start_ms, 61_250);
        assert_eq!(segment.end_ms, 63_500);
        assert_eq!(segment.speaker, Speaker::Others);
        assert_eq!(segment.received_at, Duration::from_secs(3));
    }

    #[test]
    fn partial_empty_and_malformed_events_are_ignored() {
        let partial = r#"{"text":"Ship","source":"mic","is_partial":true,"audio_start_time":1,"audio_end_time":2}"#;
        assert!(segment_from_event(partial, Duration::ZERO).is_none());
        let empty = r#"{"text":"  ","source":"mic","audio_start_time":1,"audio_end_time":2}"#;
        assert!(segment_from_event(empty, Duration::ZERO).is_none());
        assert!(segment_from_event("not json", Duration::ZERO).is_none());
        let reversed =
            r#"{"text":"ok then","source":"mic","audio_start_time":5,"audio_end_time":-1}"#;
        let segment = segment_from_event(reversed, Duration::ZERO).unwrap();
        assert_eq!(
            (segment.start_ms, segment.end_ms, segment.speaker),
            (5000, 5000, Speaker::You)
        );
    }
}
