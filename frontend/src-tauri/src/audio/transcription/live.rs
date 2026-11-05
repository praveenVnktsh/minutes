//! The live transcription plan of the current recording: Nemotron streaming in
//! the speech helper, or no live text. The engine is loaded before capture,
//! since the pipeline fixes its mode when it starts, and the worker that runs
//! it is started here.
//!
//! Nothing here fails a recording. A missing or broken model means no live
//! text, and the user gets a notice; the final pass after the meeting
//! produces the kept transcript either way.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use log::{info, warn};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::audio::live_streaming::{self, engine::StreamingEngine};
use crate::audio::model_provenance::TranscriptionModel;
use crate::audio::pipeline::{LIVE_STREAMING_MODE, LIVE_TRANSCRIPTION_ENABLED};
use crate::audio::AudioChunk;

/// Provenance provider of live transcripts.
const STREAMING_PROVIDER: &str = "nemotron-streaming";

const NOT_DOWNLOADED_NOTICE: &str = "The live model is not downloaded yet, so this meeting is recorded without live text. It is transcribed afterwards.";

/// How live transcription runs for one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LivePlan {
    /// Nemotron streaming on continuous audio.
    Streaming,
    /// No live engine; record without live text. An empty notice is not shown.
    Unavailable { notice: String },
}

impl LivePlan {
    /// The plan before anything is loaded.
    fn initial(live_ready: bool) -> Self {
        if live_ready {
            Self::Streaming
        } else {
            Self::Unavailable {
                notice: NOT_DOWNLOADED_NOTICE.to_string(),
            }
        }
    }

    fn notice(&self) -> Option<&str> {
        match self {
            Self::Streaming => None,
            Self::Unavailable { notice } if notice.is_empty() => None,
            Self::Unavailable { notice } => Some(notice),
        }
    }
}

struct SessionPlan {
    plan: LivePlan,
    engine: Option<Arc<StreamingEngine>>,
}

static SESSION_PLAN: Mutex<Option<SessionPlan>> = Mutex::new(None);

fn take_session() -> Option<SessionPlan> {
    SESSION_PLAN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

fn store_session(session: SessionPlan) {
    *SESSION_PLAN.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
}

/// Tell the user about a degraded live path without failing anything. The
/// frontend shows non-actionable "active" transcription errors as a warning.
fn notify<R: Runtime>(app: &AppHandle<R>, message: &str) {
    warn!("Live transcription notice: {message}");
    let _ = app.emit(
        "transcription-error",
        serde_json::json!({
            "error": message,
            "userMessage": message,
            "actionable": false,
            "phase": "active"
        }),
    );
}

async fn load_streaming<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<Arc<StreamingEngine>> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || live_streaming::engine::get_or_load(&app))
        .await
        .map_err(|e| anyhow::anyhow!("Live model load panicked: {e}"))?
}

/// Load `plan`'s engine. A failed load turns the plan into no live text, so
/// the returned plan is what will run.
async fn load_plan<R: Runtime>(
    app: &AppHandle<R>,
    plan: LivePlan,
) -> (LivePlan, Option<Arc<StreamingEngine>>) {
    if plan != LivePlan::Streaming {
        return (plan, None);
    }
    match load_streaming(app).await {
        Ok(engine) => (plan, Some(engine)),
        Err(error) => {
            warn!("Nemotron live model failed to load: {error:#}");
            (
                LivePlan::Unavailable {
                    notice: format!(
                        "Live text is off for this meeting: the live model could not be loaded ({error}). The recording continues and is transcribed afterwards."
                    ),
                },
                None,
            )
        }
    }
}

/// Decide (and, when live text is on, load) the live path for a recording
/// about to start. Sets the pipeline's streaming mode to match. Never fails.
pub async fn prepare_for_recording<R: Runtime>(app: &AppHandle<R>) {
    let plan = LivePlan::initial(crate::audio::speech::models::live_ready(app));

    let live_enabled = LIVE_TRANSCRIPTION_ENABLED.load(Ordering::SeqCst);
    let (plan, engine) = if live_enabled {
        load_plan(app, plan).await
    } else {
        (plan, None)
    };

    info!(
        "Live transcription plan: {} (live text {})",
        match &plan {
            LivePlan::Streaming => "Nemotron streaming",
            LivePlan::Unavailable { .. } => "unavailable",
        },
        if live_enabled { "on" } else { "deferred" }
    );
    // With live text deferred, a streaming plan still sets the mode, so live
    // text turned on mid-meeting can load the engine then.
    LIVE_STREAMING_MODE.store(plan == LivePlan::Streaming, Ordering::SeqCst);
    if live_enabled {
        if let Some(notice) = plan.notice() {
            notify(app, notice);
        }
    }
    store_session(SessionPlan { plan, engine });
}

fn provenance(model_id: &str) -> TranscriptionModel {
    TranscriptionModel {
        provider: STREAMING_PROVIDER.to_string(),
        model: model_id.to_string(),
    }
}

/// Start the live worker for this recording's plan, loading its engine if
/// that was deferred. Returns the worker and the model for provenance, or
/// gives the receiver back when there is no live engine (the caller keeps it
/// as in deferred mode, and live text is turned off so nothing piles up).
pub async fn start_worker<R: Runtime>(
    app: &AppHandle<R>,
    receiver: UnboundedReceiver<AudioChunk>,
) -> Result<(tokio::task::JoinHandle<()>, Option<TranscriptionModel>), UnboundedReceiver<AudioChunk>>
{
    let (plan, engine) = match take_session() {
        Some(SessionPlan {
            plan,
            engine: Some(engine),
        }) => (plan, Some(engine)),
        Some(SessionPlan { plan, engine: None }) => {
            // Live text was enabled mid-meeting. The pipeline's mode is fixed,
            // so only a streaming plan can load an engine now.
            let (plan, engine) = load_plan(app, plan).await;
            if let Some(notice) = plan.notice() {
                notify(app, notice);
            }
            (plan, engine)
        }
        None => {
            warn!("No live plan prepared for this recording; preparing one now");
            prepare_for_recording(app).await;
            match take_session() {
                Some(SessionPlan { plan, engine }) => (plan, engine),
                None => (
                    LivePlan::Unavailable {
                        notice: String::new(),
                    },
                    None,
                ),
            }
        }
    };

    let outcome = match engine {
        Some(engine) => {
            let model = provenance(engine.model_id());
            Ok((
                live_streaming::start_streaming_transcription_task(app.clone(), receiver, engine),
                Some(model),
            ))
        }
        None => {
            LIVE_TRANSCRIPTION_ENABLED.store(false, Ordering::SeqCst);
            Err(receiver)
        }
    };
    // Keep the plan (without the engine handle) in case live text is turned
    // off and on again during this recording.
    store_session(SessionPlan { plan, engine: None });
    outcome
}

/// Drop what [`prepare_for_recording`] loaded for a recording that then
/// failed to start. Nothing else would release it, since the stop path only
/// runs for a started recording.
pub fn abandon_prepared() {
    let session = take_session();
    LIVE_STREAMING_MODE.store(false, Ordering::SeqCst);
    if let Some(engine) = session.and_then(|s| s.engine) {
        info!("Recording did not start: releasing the prepared live engine");
        live_streaming::engine::drop_in_background(engine);
    }
    live_streaming::engine::unload();
}

/// Releases the prepared live engine on drop unless [`Self::disarm`]ed, so
/// every early return of a recording start cleans up.
pub struct PreparedLive {
    armed: bool,
}

impl PreparedLive {
    pub fn new() -> Self {
        Self { armed: true }
    }

    /// The recording started: its stop path owns the engine from here.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Default for PreparedLive {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreparedLive {
    fn drop(&mut self) {
        if self.armed {
            abandon_prepared();
        }
    }
}

/// Free the live engine after a recording, so the final pass has the memory.
pub fn release_after_recording() {
    if let Some(session) = take_session() {
        live_streaming::engine::drop_in_background(session);
    }
    live_streaming::engine::unload();
    LIVE_STREAMING_MODE.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_downloaded_live_model_streams() {
        assert_eq!(LivePlan::initial(true), LivePlan::Streaming);
        assert_eq!(LivePlan::initial(true).notice(), None);
    }

    #[test]
    fn without_the_live_model_the_meeting_is_recorded_without_live_text() {
        let plan = LivePlan::initial(false);
        assert!(matches!(plan, LivePlan::Unavailable { .. }));
        let notice = plan.notice().expect("the user is told why");
        assert!(notice.contains("not downloaded"), "{notice}");
        assert!(notice.contains("transcribed afterwards"), "{notice}");
    }

    #[test]
    fn an_empty_notice_is_not_shown() {
        let plan = LivePlan::Unavailable {
            notice: String::new(),
        };
        assert_eq!(plan.notice(), None);
    }

    #[test]
    fn live_transcripts_are_attributed_to_nemotron_streaming() {
        let model = provenance("nemotron-speech-streaming-en-0.6b-coreml");
        assert_eq!(model.provider, "nemotron-streaming");
        assert_eq!(model.model, "nemotron-speech-streaming-en-0.6b-coreml");
    }
}
