//! The automatic final transcript pass: after a recording is saved, re-run the
//! final model over the whole recording through the transcription queue. The
//! result replaces the live transcript and diarization reruns on it.
//!
//! Every outcome ends with [`COMPLETE_EVENT`], the hook for anything that
//! should run on the kept transcript (the auto-summary, and the planned
//! summarization rework):
//! `{ meeting_id, task_id, outcome: "replaced" | "kept_live", provider, model, warning }`.

use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::transcription_queue::{get_queue, TaskStatus, TaskType, TranscriptionTask};
use super::transcription_settings::{self as settings, FinalPassDecision, FinalPassSkip};

pub const COMPLETE_EVENT: &str = "final-transcript-pass-complete";
pub const TASK_TITLE: &str = "Final transcript";

#[derive(Debug, Clone, Serialize)]
pub struct FinalPassComplete {
    pub meeting_id: String,
    pub task_id: String,
    /// "replaced": the final transcript is now the meeting's transcript.
    /// "kept_live": the pass failed or was cancelled; the live one stays.
    pub outcome: &'static str,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalPassStarted {
    /// "queued" or "skipped".
    pub status: &'static str,
    pub task_id: Option<String>,
    pub skip_reason: Option<FinalPassSkip>,
    pub message: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Set when the configured final model was missing and another was used.
    pub fallback_notice: Option<String>,
}

async fn count(pool: &sqlx::SqlitePool, sql: &str, meeting_id: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .bind(meeting_id)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|error| {
            warn!("Final pass: count query failed: {error}");
            0
        })
}

/// Queue the final pass for a saved meeting, or say why not. `force` runs it
/// even when the automatic pass is off (deferred mode, where it is the only
/// transcription).
#[tauri::command]
pub async fn start_final_transcript_pass<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    force: Option<bool>,
) -> Result<FinalPassStarted, String> {
    let state = app
        .try_state::<crate::state::AppState>()
        .ok_or("App state not available")?;
    let pool = state.db_manager.pool();
    let has_revisions = count(
        pool,
        "SELECT COUNT(*) FROM transcript_revisions WHERE meeting_id = ?",
        &meeting_id,
    )
    .await
        > 0;
    let has_transcripts = count(
        pool,
        "SELECT COUNT(*) FROM transcripts WHERE meeting_id = ?",
        &meeting_id,
    )
    .await
        > 0;

    let current = settings::load_settings(&app);
    let preferred = settings::configured_final_model(&app).await;
    let available = settings::availability().await;
    let engine = settings::choose_final_engine(&preferred, &available);

    match settings::decide_final_pass(
        current.auto_final_pass,
        force.unwrap_or(false),
        has_revisions,
        has_transcripts,
        engine,
    ) {
        FinalPassDecision::Skip(reason) => {
            info!("Final pass skipped for {meeting_id}: {reason:?}");
            Ok(FinalPassStarted {
                status: "skipped",
                task_id: None,
                skip_reason: Some(reason),
                message: Some(reason.message().to_string()),
                provider: None,
                model: None,
                fallback_notice: None,
            })
        }
        FinalPassDecision::Run(choice) => {
            let task = TranscriptionTask {
                task_id: String::new(),
                task_type: TaskType::Retranscribe,
                title: TASK_TITLE.to_string(),
                status: TaskStatus::Pending,
                source_path: None,
                meeting_id: Some(meeting_id.clone()),
                meeting_folder_path: Some(meeting_folder_path),
                language,
                model: Some(choice.engine.model.clone()),
                provider: Some(choice.engine.provider.clone()),
                final_pass: true,
            };
            let task_id = get_queue().enqueue(&app, task).await;
            info!(
                "Final pass queued for {meeting_id} with {} (task {task_id})",
                choice.engine.display_name()
            );
            Ok(FinalPassStarted {
                status: "queued",
                task_id: Some(task_id),
                skip_reason: None,
                message: None,
                provider: Some(choice.engine.provider),
                model: Some(choice.engine.model),
                fallback_notice: choice.fallback_notice,
            })
        }
    }
}

pub fn emit_complete<R: Runtime>(app: &AppHandle<R>, event: FinalPassComplete) {
    info!(
        "Final pass for {} finished: {} ({:?})",
        event.meeting_id, event.outcome, event.warning
    );
    if let Err(error) = app.emit(COMPLETE_EVENT, &event) {
        warn!("Failed to emit {COMPLETE_EVENT}: {error}");
    }
}

/// The final pass failed or was cancelled. The live transcript was never
/// touched (rows are only replaced in the pass's final transaction), so keep
/// it: diarize it as the live path would have, and mark the task ready with a
/// warning so the meeting still gets its speakers and summary.
pub async fn keep_live_transcript<R: Runtime>(
    app: &AppHandle<R>,
    task: &TranscriptionTask,
    meeting_id: &str,
    reason: &str,
) {
    let mut warning = format!("Kept the live transcript: {reason}");
    if let Err(error) = super::diarization::run_for_meeting(app, meeting_id, None).await {
        warning.push_str(&format!(". Speaker identification unavailable: {error}"));
    }
    crate::meeting_activity::set_task_warning(app, &task.task_id, Some(warning.clone()));
    crate::meeting_activity::update_task(
        app,
        &task.task_id,
        crate::meeting_activity::ActivityStatus::Ready,
        Some(meeting_id.to_string()),
        Some("complete".to_string()),
        Some(100),
        Some(warning.clone()),
        None,
    );
    super::common::emit_transcripts_updated(app, meeting_id);
    emit_complete(
        app,
        FinalPassComplete {
            meeting_id: meeting_id.to_string(),
            task_id: task.task_id.clone(),
            outcome: "kept_live",
            provider: task.provider.clone(),
            model: task.model.clone(),
            warning: Some(warning),
        },
    );
}
