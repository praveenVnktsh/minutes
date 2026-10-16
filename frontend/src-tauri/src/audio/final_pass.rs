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

/// A failed count is an error, never zero: zero revisions or transcripts would
/// let the pass run when it must not (over an edited transcript, or with the
/// automatic pass turned off).
async fn count(pool: &sqlx::SqlitePool, sql: &str, meeting_id: &str) -> Result<i64, String> {
    sqlx::query_scalar::<_, i64>(sql)
        .bind(meeting_id)
        .fetch_one(pool)
        .await
        .map_err(|error| {
            warn!("Final pass: count query failed: {error}");
            format!("Could not check the meeting's transcript before the final pass: {error}")
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
    queue_final_pass(
        &app,
        meeting_id,
        meeting_folder_path,
        language,
        force.unwrap_or(false),
    )
    .await
}

async fn queue_final_pass<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    force: bool,
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
    .await?
        > 0;
    let has_transcripts = count(
        pool,
        "SELECT COUNT(*) FROM transcripts WHERE meeting_id = ?",
        &meeting_id,
    )
    .await?
        > 0;

    let current = settings::load_settings(app);
    let preferred = settings::configured_final_model(app).await;
    let available = settings::availability().await;
    let engine = settings::choose_final_engine(&preferred, &available);

    match settings::decide_final_pass(
        current.auto_final_pass,
        force,
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
            // Durable before it is queued: the queue is in memory, and a quit
            // before the pass replaces the transcript must not lose it.
            if let Err(error) = mark_queued(
                pool,
                &meeting_id,
                &meeting_folder_path,
                language.as_deref(),
                force,
            )
            .await
            {
                warn!("Final pass: could not record the queued pass for {meeting_id}: {error}");
            }
            let task = TranscriptionTask {
                task_id: String::new(),
                task_type: TaskType::Retranscribe,
                title: TASK_TITLE.to_string(),
                status: TaskStatus::Pending,
                source_path: None,
                meeting_id: Some(meeting_id.clone()),
                meeting_folder_path: Some(meeting_folder_path.clone()),
                language,
                model: Some(choice.engine.model.clone()),
                provider: Some(choice.engine.provider.clone()),
                final_pass: true,
            };
            let task_id = get_queue().enqueue(app, task).await;
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
    // The pass is over: hand the meeting to the diarization resume (in one
    // transaction) before diarizing, so a quit from here on still gets the
    // live transcript its speakers.
    if let Some(state) = app.try_state::<crate::state::AppState>() {
        if let Err(error) = settle_kept_live(state.db_manager.pool(), meeting_id).await {
            warn!("Final pass: could not settle the pending pass for {meeting_id}: {error}");
        }
    }
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

// ---------------------------------------------------------------------------
// Surviving a quit: the queue is in memory, the pending pass is not
// ---------------------------------------------------------------------------

/// Only resume passes queued within this window; older rows are dropped.
const RESUME_WINDOW_DAYS: i64 = 7;
/// Resume a pass at most this many times, so a pass that crashes the app
/// cannot crash every launch after it.
const RESUME_MAX_ATTEMPTS: i64 = 3;
/// Wait before resuming, like the diarization resume, so startup is not
/// competing with a model load.
const RESUME_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct PendingFinalPass {
    pub meeting_id: String,
    pub meeting_folder_path: String,
    pub language: Option<String>,
    pub force: bool,
    /// Times startup has already resumed this pass.
    pub attempts: i64,
}

/// Record a queued pass. Re-queuing the same meeting refreshes the row.
pub(crate) async fn mark_queued(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    meeting_folder_path: &str,
    language: Option<&str>,
    force: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO pending_final_passes (meeting_id, meeting_folder_path, language, force, queued_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET meeting_folder_path = excluded.meeting_folder_path, \
         language = excluded.language, force = excluded.force, queued_at = excluded.queued_at",
    )
    .bind(meeting_id)
    .bind(meeting_folder_path)
    .bind(language)
    .bind(force)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// The pass replaced the transcript: called inside that transaction, so the
/// pass can never run twice for one recording.
pub(crate) async fn clear_in_tx(
    conn: &mut sqlx::SqliteConnection,
    meeting_id: &str,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM pending_final_passes WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// The pass gave up and the live transcript stays: it owes speakers, which
/// the diarization resume covers from here.
async fn settle_kept_live(pool: &sqlx::SqlitePool, meeting_id: &str) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    super::diarization::mark_pending_in_tx(&mut tx, meeting_id).await?;
    clear_in_tx(&mut tx, meeting_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Passes that were queued when the app last stopped, oldest first.
pub(crate) async fn interrupted_passes(
    pool: &sqlx::SqlitePool,
) -> sqlx::Result<Vec<PendingFinalPass>> {
    let since = (chrono::Utc::now() - chrono::Duration::days(RESUME_WINDOW_DAYS)).to_rfc3339();
    sqlx::query_as::<_, PendingFinalPass>(
        "SELECT meeting_id, meeting_folder_path, language, force, attempts FROM pending_final_passes \
         WHERE queued_at >= ? ORDER BY queued_at",
    )
    .bind(since)
    .fetch_all(pool)
    .await
}

/// Drop pending passes too old to resume: the live transcript stays, as it
/// already is, and nothing would ever clear these rows otherwise.
pub(crate) async fn drop_stale_passes(pool: &sqlx::SqlitePool) -> sqlx::Result<u64> {
    let since = (chrono::Utc::now() - chrono::Duration::days(RESUME_WINDOW_DAYS)).to_rfc3339();
    Ok(
        sqlx::query("DELETE FROM pending_final_passes WHERE queued_at < ?")
            .bind(since)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

/// Count a resume before queuing it, so a pass that kills the app mid-run is
/// counted even though it never finishes.
pub(crate) async fn record_resume_attempt(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE pending_final_passes SET attempts = attempts + 1 WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Queue again the final passes a quit or crash interrupted. A pass that can
/// no longer run (turned off, edited, no model) keeps the live transcript and
/// still gets its speakers and a ready task, which starts the auto-summary.
pub fn init_resume_worker<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(RESUME_DELAY).await;
        let Some(pool) = app
            .try_state::<crate::state::AppState>()
            .map(|state| state.db_manager.pool().clone())
        else {
            return;
        };
        match drop_stale_passes(&pool).await {
            Ok(0) => {}
            Ok(dropped) => info!("Final pass resume: dropped {dropped} pass(es) too old to resume"),
            Err(error) => warn!("Final pass resume: could not drop stale passes: {error}"),
        }
        let pending = match interrupted_passes(&pool).await {
            Ok(pending) => pending,
            Err(error) => {
                warn!("Final pass resume: could not list interrupted passes: {error}");
                return;
            }
        };
        for pass in pending {
            let outcome = if pass.attempts >= RESUME_MAX_ATTEMPTS {
                warn!(
                    "Final pass resume: the pass for {} was interrupted {} times, keeping the live transcript",
                    pass.meeting_id, pass.attempts
                );
                Err(format!(
                    "the final pass was interrupted {} times, so the live transcript was kept",
                    pass.attempts
                ))
            } else {
                info!(
                    "Final pass resume: the pass for {} was interrupted, queuing it again",
                    pass.meeting_id
                );
                if let Err(error) = record_resume_attempt(&pool, &pass.meeting_id).await {
                    // Without the count a crashing pass could loop; skip it this launch.
                    warn!(
                        "Final pass resume: could not count the attempt for {}: {error}",
                        pass.meeting_id
                    );
                    continue;
                }
                match queue_final_pass(
                    &app,
                    pass.meeting_id.clone(),
                    pass.meeting_folder_path.clone(),
                    pass.language.clone(),
                    pass.force,
                )
                .await
                {
                    Ok(started) if started.status == "queued" => Ok(()),
                    Ok(started) => Err(started
                        .message
                        .unwrap_or_else(|| "the interrupted final pass could not run".into())),
                    // Left in place: the next start tries again (within the window
                    // and the attempt limit).
                    Err(error) => {
                        warn!(
                            "Final pass resume: could not requeue the pass for {}: {error}",
                            pass.meeting_id
                        );
                        Ok(())
                    }
                }
            };
            match outcome {
                Ok(()) => {}
                Err(reason) => {
                    let task = TranscriptionTask {
                        task_id: format!("task-{}", uuid::Uuid::new_v4()),
                        task_type: TaskType::Retranscribe,
                        title: TASK_TITLE.to_string(),
                        status: TaskStatus::Pending,
                        source_path: None,
                        meeting_id: Some(pass.meeting_id.clone()),
                        meeting_folder_path: Some(pass.meeting_folder_path.clone()),
                        language: pass.language.clone(),
                        model: None,
                        provider: None,
                        final_pass: true,
                    };
                    crate::meeting_activity::register_task(
                        &app,
                        task.task_id.clone(),
                        task.meeting_id.clone(),
                        crate::meeting_activity::ActivityKind::Retranscription,
                        task.title.clone(),
                    );
                    keep_live_transcript(&app, &task, &pass.meeting_id, &reason).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Executor;

    async fn pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        pool.execute(
            "PRAGMA foreign_keys = ON; \
             CREATE TABLE meetings (id TEXT PRIMARY KEY NOT NULL); \
             INSERT INTO meetings VALUES ('m1'), ('m2');",
        )
        .await
        .unwrap();
        pool.execute(include_str!(
            "../../migrations/20260924000000_add_diarization_state.sql"
        ))
        .await
        .unwrap();
        pool.execute(include_str!(
            "../../migrations/20261016000100_add_pending_final_passes.sql"
        ))
        .await
        .unwrap();
        pool.execute(include_str!(
            "../../migrations/20261016000200_add_pending_final_pass_attempts.sql"
        ))
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn resume_attempts_survive_requeue_and_stale_passes_are_dropped() {
        let pool = pool().await;
        mark_queued(&pool, "m1", "/tmp/m1", None, false)
            .await
            .unwrap();
        record_resume_attempt(&pool, "m1").await.unwrap();
        record_resume_attempt(&pool, "m1").await.unwrap();
        // The resume re-queues through mark_queued, which must keep the count.
        mark_queued(&pool, "m1", "/tmp/m1", None, false)
            .await
            .unwrap();
        let pending = interrupted_passes(&pool).await.unwrap();
        assert_eq!(pending[0].attempts, 2);

        // A pass queued before the resume window is dropped, not left forever.
        mark_queued(&pool, "m2", "/tmp/m2", None, false)
            .await
            .unwrap();
        let old =
            (chrono::Utc::now() - chrono::Duration::days(RESUME_WINDOW_DAYS + 1)).to_rfc3339();
        sqlx::query("UPDATE pending_final_passes SET queued_at = ? WHERE meeting_id = 'm2'")
            .bind(old)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(drop_stale_passes(&pool).await.unwrap(), 1);
        let left: Vec<String> = sqlx::query_scalar("SELECT meeting_id FROM pending_final_passes")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, vec!["m1".to_string()]);
    }

    #[tokio::test]
    async fn a_queued_pass_survives_until_the_transcript_is_replaced() {
        let pool = pool().await;
        mark_queued(&pool, "m1", "/tmp/m1", Some("en"), false)
            .await
            .unwrap();
        mark_queued(&pool, "m2", "/tmp/m2", None, true)
            .await
            .unwrap();
        // Re-queuing the same meeting keeps one row.
        mark_queued(&pool, "m1", "/tmp/m1", Some("de"), false)
            .await
            .unwrap();

        let pending = interrupted_passes(&pool).await.unwrap();
        assert_eq!(pending.len(), 2);
        let m1 = pending.iter().find(|p| p.meeting_id == "m1").unwrap();
        assert_eq!(m1.language.as_deref(), Some("de"));
        assert!(pending.iter().find(|p| p.meeting_id == "m2").unwrap().force);

        // The pass's replace transaction clears it: no second run at startup.
        let mut tx = pool.begin().await.unwrap();
        clear_in_tx(&mut tx, "m1").await.unwrap();
        tx.commit().await.unwrap();
        let pending = interrupted_passes(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].meeting_id, "m2");
    }

    #[tokio::test]
    async fn keeping_the_live_transcript_hands_it_to_the_diarization_resume() {
        let pool = pool().await;
        mark_queued(&pool, "m1", "/tmp/m1", None, false)
            .await
            .unwrap();
        settle_kept_live(&pool, "m1").await.unwrap();
        assert!(interrupted_passes(&pool).await.unwrap().is_empty());
        let status: String =
            sqlx::query_scalar("SELECT status FROM diarization_state WHERE meeting_id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn deleting_the_meeting_drops_its_pending_pass() {
        let pool = pool().await;
        mark_queued(&pool, "m1", "/tmp/m1", None, false)
            .await
            .unwrap();
        sqlx::query("DELETE FROM meetings WHERE id = 'm1'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(interrupted_passes(&pool).await.unwrap().is_empty());
    }
}
