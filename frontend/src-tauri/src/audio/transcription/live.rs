//! The live transcription plan of the current recording: which model streams
//! text, loaded before capture so a load failure can still fall back, and the
//! worker that runs it.
//!
//! Nothing here fails a recording. A missing or broken model degrades to the
//! chunked engine or to no live text, and the user gets a notice; the final
//! pass after the meeting produces the kept transcript either way.

use std::sync::{Arc, Mutex};

use log::{info, warn};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::mpsc::UnboundedReceiver;

use super::engine::TranscriptionEngine;
use crate::audio::live_streaming::{self, engine::StreamingEngine};
use crate::audio::model_provenance::TranscriptionModel;
use crate::audio::pipeline::{LIVE_STREAMING_MODE, LIVE_TRANSCRIPTION_ENABLED};
use crate::audio::transcription_settings::{self as settings, EngineChoice, LivePlan};
use crate::audio::AudioChunk;
use std::sync::atomic::Ordering;

enum Loaded {
    Streaming(Arc<StreamingEngine>),
    Chunked(TranscriptionEngine),
}

struct SessionPlan {
    plan: LivePlan,
    loaded: Option<Loaded>,
}

static SESSION_PLAN: Mutex<Option<SessionPlan>> = Mutex::new(None);

/// Provider of the chunked engine a live recording is transcribing with right
/// now, if any. Whisper and Parakeet engines are process-wide, so a final pass
/// on the same engine family must not run alongside it (it would swap the
/// loaded model under the recording).
static CHUNKED_LIVE_PROVIDER: Mutex<Option<String>> = Mutex::new(None);

pub fn chunked_live_provider() -> Option<String> {
    CHUNKED_LIVE_PROVIDER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Provider of a chunked engine loaded for a recording that is about to start
/// (prepared, worker not running yet).
pub fn prepared_chunked_provider() -> Option<String> {
    let session = SESSION_PLAN.lock().unwrap_or_else(|e| e.into_inner());
    match session.as_ref() {
        Some(SessionPlan {
            plan: LivePlan::Chunked(choice),
            loaded: Some(Loaded::Chunked(_)),
        }) => Some(choice.engine.provider.clone()),
        _ => None,
    }
}

fn is_parakeet(provider: &str) -> bool {
    provider == settings::PROVIDER_PARAKEET
}

/// Whether a pass on `pass_provider` would share an engine with the live
/// recording's chunked engine. The streaming (Nemotron) recognizer is separate
/// and never conflicts.
pub fn live_conflicts(live_provider: Option<&str>, pass_provider: Option<&str>) -> bool {
    match (live_provider, pass_provider) {
        (Some(live), Some(pass)) => is_parakeet(live) == is_parakeet(pass),
        (Some(live), None) => !is_parakeet(live), // a pass without a provider uses Whisper
        _ => false,
    }
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

async fn load_chunked<R: Runtime>(
    app: &AppHandle<R>,
    choice: &EngineChoice,
) -> anyhow::Result<TranscriptionEngine> {
    let model = Some(choice.engine.model.as_str());
    if choice.engine.is_parakeet() {
        crate::parakeet_engine::commands::parakeet_init()
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(TranscriptionEngine::Parakeet(
            crate::audio::retranscription::get_or_init_parakeet(app, model).await?,
        ))
    } else {
        crate::whisper_engine::commands::whisper_init()
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(TranscriptionEngine::Whisper(
            crate::audio::retranscription::get_or_init_whisper(app, model).await?,
        ))
    }
}

async fn load_streaming<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<Arc<StreamingEngine>> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || live_streaming::engine::get_or_load(&app))
        .await
        .map_err(|e| anyhow::anyhow!("Live model load panicked: {e}"))?
}

/// Load `plan`'s engine. A failed load falls back down the plan ladder
/// (streaming → chunked → none), so the returned plan is what will run.
async fn load_plan<R: Runtime>(app: &AppHandle<R>, plan: LivePlan) -> (LivePlan, Option<Loaded>) {
    let mut plan = plan;
    if plan == LivePlan::Streaming {
        match load_streaming(app).await {
            Ok(engine) => return (plan, Some(Loaded::Streaming(engine))),
            Err(error) => {
                warn!("Nemotron live model failed to load: {error:#}");
                let final_model = settings::configured_final_model(app).await;
                let available = settings::availability().await;
                plan = settings::resolve_live_plan(
                    settings::LiveModelChoice::NemotronStreaming,
                    false,
                    &final_model,
                    &available,
                );
            }
        }
    }
    if let LivePlan::Chunked(choice) = &plan {
        match load_chunked(app, choice).await {
            Ok(engine) => return (plan, Some(Loaded::Chunked(engine))),
            Err(error) => {
                warn!(
                    "Live engine {} failed to load: {error:#}",
                    choice.engine.display_name()
                );
                plan = LivePlan::Unavailable {
                    notice: format!(
                        "Live text is off for this meeting: {} could not be loaded. The recording continues and is transcribed afterwards.",
                        choice.engine.display_name()
                    ),
                };
            }
        }
    }
    (plan, None)
}

fn plan_notice(plan: &LivePlan) -> Option<String> {
    match plan {
        LivePlan::Streaming => None,
        LivePlan::Chunked(choice) => choice.fallback_notice.clone(),
        LivePlan::Unavailable { notice } => Some(notice.clone()),
    }
}

/// Decide (and, when live text is on, load) the live path for a recording
/// about to start. Sets the pipeline's streaming mode to match. Never fails.
pub async fn prepare_for_recording<R: Runtime>(app: &AppHandle<R>) {
    let current = settings::load_settings(app);
    let nemotron_ready = live_streaming::SUPPORTED && live_streaming::model::is_downloaded(app);
    let final_model = settings::configured_final_model(app).await;
    let available = settings::availability().await;
    let plan =
        settings::resolve_live_plan(current.live_model, nemotron_ready, &final_model, &available);

    let live_enabled = LIVE_TRANSCRIPTION_ENABLED.load(Ordering::SeqCst);
    let (plan, loaded) = if live_enabled {
        load_plan(app, plan).await
    } else {
        (plan, None)
    };

    info!(
        "Live transcription plan: {} (live text {})",
        match &plan {
            LivePlan::Streaming => "Nemotron streaming".to_string(),
            LivePlan::Chunked(choice) => format!("chunked {}", choice.engine.display_name()),
            LivePlan::Unavailable { .. } => "unavailable".to_string(),
        },
        if live_enabled { "on" } else { "deferred" }
    );
    LIVE_STREAMING_MODE.store(plan == LivePlan::Streaming, Ordering::SeqCst);
    if live_enabled {
        if let Some(notice) = plan_notice(&plan) {
            notify(app, &notice);
        }
    }
    *SESSION_PLAN.lock().unwrap_or_else(|e| e.into_inner()) = Some(SessionPlan { plan, loaded });
}

fn provenance(plan: &LivePlan) -> Option<TranscriptionModel> {
    match plan {
        LivePlan::Streaming => Some(TranscriptionModel {
            provider: "nemotron-streaming".to_string(),
            model: live_streaming::model::MODEL_ID.to_string(),
        }),
        LivePlan::Chunked(choice) => Some(TranscriptionModel {
            provider: if choice.engine.is_parakeet() {
                "parakeet"
            } else {
                "whisper"
            }
            .to_string(),
            model: choice.engine.model.clone(),
        }),
        LivePlan::Unavailable { .. } => None,
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
    let session = SESSION_PLAN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let (plan, loaded) = match session {
        Some(SessionPlan {
            plan,
            loaded: Some(loaded),
        }) => (plan, Some(loaded)),
        Some(SessionPlan { plan, loaded: None }) => {
            // Live text was enabled mid-meeting. The pipeline's mode is fixed,
            // so a streaming plan may only fall back to nothing here.
            let was_streaming = plan == LivePlan::Streaming;
            let (mut plan, loaded) = load_plan(app, plan).await;
            if was_streaming && plan != LivePlan::Streaming {
                plan = LivePlan::Unavailable {
                    notice: "The Nemotron live model could not be loaded, so live text stays off for this meeting. It is transcribed afterwards.".to_string(),
                };
            }
            if let Some(notice) = plan_notice(&plan) {
                notify(app, &notice);
            }
            let loaded = if matches!(plan, LivePlan::Unavailable { .. }) {
                None
            } else {
                loaded
            };
            (plan, loaded)
        }
        None => {
            warn!("No live plan prepared for this recording; preparing one now");
            prepare_for_recording(app).await;
            let retry = SESSION_PLAN
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            match retry {
                Some(SessionPlan { plan, loaded }) => (plan, loaded),
                None => (
                    LivePlan::Unavailable {
                        notice: String::new(),
                    },
                    None,
                ),
            }
        }
    };

    let model = provenance(&plan);
    let outcome = match loaded {
        Some(Loaded::Streaming(engine)) => Ok((
            live_streaming::start_streaming_transcription_task(app.clone(), receiver, engine),
            model,
        )),
        Some(Loaded::Chunked(engine)) => {
            if let LivePlan::Chunked(choice) = &plan {
                *CHUNKED_LIVE_PROVIDER
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(choice.engine.provider.clone());
            }
            Ok((
                super::worker::start_transcription_task_with_engine(app.clone(), receiver, engine),
                model,
            ))
        }
        None => {
            LIVE_TRANSCRIPTION_ENABLED.store(false, Ordering::SeqCst);
            Err(receiver)
        }
    };
    // Keep the plan (without the engine handle) for the stop path's unload.
    *SESSION_PLAN.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(SessionPlan { plan, loaded: None });
    outcome
}

/// Drop what [`prepare_for_recording`] loaded for a recording that then
/// failed to start: the Nemotron recognizer cache, or the chunked engine's
/// model (unless a queued task is using the engines). Nothing else would
/// release them, since the stop path only runs for a started recording.
pub fn abandon_prepared() {
    let session = SESSION_PLAN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    LIVE_STREAMING_MODE.store(false, Ordering::SeqCst);
    live_streaming::engine::unload();
    if let Some(Loaded::Chunked(engine)) = session.and_then(|s| s.loaded) {
        let parakeet = matches!(engine, TranscriptionEngine::Parakeet(_));
        drop(engine);
        info!("Recording did not start: releasing the prepared live engine");
        tauri::async_runtime::spawn(async move {
            if crate::audio::transcription_queue::get_queue()
                .has_active_task()
                .await
            {
                return;
            }
            crate::audio::common::unload_engine_after_batch(parakeet).await;
        });
    }
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
/// Returns the provider of the chunked engine the recording used, if any, for
/// the stop path to unload; a streaming recording loaded none.
pub fn release_after_recording() -> Option<String> {
    live_streaming::engine::unload();
    LIVE_STREAMING_MODE.store(false, Ordering::SeqCst);
    CHUNKED_LIVE_PROVIDER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_streaming_or_idle_recording_never_conflicts_with_a_pass() {
        assert!(!live_conflicts(None, Some("localWhisper")));
        assert!(!live_conflicts(None, Some("parakeet")));
        assert!(!live_conflicts(None, None));
    }

    #[test]
    fn only_the_same_engine_family_conflicts() {
        assert!(live_conflicts(Some("localWhisper"), Some("localWhisper")));
        assert!(live_conflicts(Some("parakeet"), Some("parakeet")));
        assert!(live_conflicts(Some("localWhisper"), None));
        assert!(!live_conflicts(Some("parakeet"), Some("localWhisper")));
        assert!(!live_conflicts(Some("localWhisper"), Some("parakeet")));
        assert!(!live_conflicts(Some("parakeet"), None));
    }
}
