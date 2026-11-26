use crate::api::{MeetingDetails, MeetingTranscript};
use crate::database::models::{MeetingModel, Transcript};
use chrono::Utc;
use sqlx::{Connection, Error as SqlxError, SqliteConnection, SqlitePool};
use tracing::{error, info};
use uuid::Uuid;

pub struct MeetingsRepository;

/// Who asked for a generated meeting title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    /// An automatic pass (the AI notes naming the meeting).
    Generated,
    /// The user clicked "Regenerate title".
    Requested,
}

/// What `update_meeting_name` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleUpdate {
    Saved,
    /// The user chose this meeting's title, so the generated one was dropped.
    Kept,
    NotFound,
}

impl MeetingsRepository {
    /// Create an empty meeting row. Used so a meeting exists before recording
    /// starts, letting the workspace own the whole recording lifecycle.
    pub async fn create_meeting(
        pool: &SqlitePool,
        title: &str,
        folder_path: Option<String>,
        is_debug: bool,
    ) -> Result<String, SqlxError> {
        let meeting_id = format!("meeting-{}", Uuid::new_v4());
        let now = Utc::now();

        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path, is_debug) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&meeting_id)
        .bind(title)
        .bind(now)
        .bind(now)
        .bind(&folder_path)
        .bind(is_debug as i64)
        .execute(pool)
        .await?;

        info!(
            "Created empty meeting {} ('{}', debug={})",
            meeting_id, title, is_debug
        );
        Ok(meeting_id)
    }

    /// Delete every meeting flagged as debug. Cascades to transcripts/summaries.
    pub async fn delete_debug_meetings(pool: &SqlitePool) -> Result<u64, SqlxError> {
        let result = sqlx::query("DELETE FROM meetings WHERE is_debug = 1")
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn get_meetings(pool: &SqlitePool) -> Result<Vec<MeetingModel>, sqlx::Error> {
        let meetings =
            sqlx::query_as::<_, MeetingModel>("SELECT * FROM meetings ORDER BY created_at DESC")
                .fetch_all(pool)
                .await?;
        Ok(meetings)
    }

    pub async fn delete_meeting(pool: &SqlitePool, meeting_id: &str) -> Result<bool, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        match delete_meeting_with_transaction(&mut transaction, meeting_id).await {
            Ok(success) => {
                if success {
                    transaction.commit().await?;
                    info!(
                        "Successfully deleted meeting {} and all associated data",
                        meeting_id
                    );
                    Ok(true)
                } else {
                    transaction.rollback().await?;
                    Ok(false)
                }
            }
            Err(e) => {
                let _ = transaction.rollback().await;
                error!("Failed to delete meeting {}: {}", meeting_id, e);
                Err(e)
            }
        }
    }

    pub async fn get_meeting(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Option<MeetingDetails>, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        // Get meeting details
        let meeting: Option<MeetingModel> = sqlx::query_as(
            "SELECT id, title, created_at, updated_at, folder_path, pinned, archived FROM meetings WHERE id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(&mut *transaction)
        .await?;

        if meeting.is_none() {
            transaction.rollback().await?;
            return Err(SqlxError::RowNotFound);
        }

        if let Some(meeting) = meeting {
            // Get all transcripts for this meeting
            let transcripts =
                sqlx::query_as::<_, Transcript>("SELECT * FROM transcripts WHERE meeting_id = ?")
                    .bind(meeting_id)
                    .fetch_all(&mut *transaction)
                    .await?;

            transaction.commit().await?;

            let resolved = crate::audio::speaker_corrections::resolve_transcript_speakers(
                pool,
                meeting_id,
                &transcripts,
            )
            .await
            .map_err(|error| SqlxError::Protocol(error.to_string()))?;

            // Convert Transcript to MeetingTranscript
            let meeting_transcripts = transcripts
                .into_iter()
                .map(|t| {
                    let speaker = resolved.get(&t.id);
                    MeetingTranscript {
                        id: t.id,
                        text: t.transcript,
                        timestamp: t.timestamp,
                        speaker: speaker
                            .map(|value| value.display_name.clone())
                            .or(t.speaker),
                        speaker_id: speaker.map(|value| value.speaker_id.clone()),
                        audio_start_time: t.audio_start_time,
                        audio_end_time: t.audio_end_time,
                        duration: t.duration,
                    }
                })
                .collect::<Vec<_>>();

            Ok(Some(MeetingDetails {
                id: meeting.id,
                title: meeting.title,
                created_at: meeting.created_at.0.to_rfc3339(),
                updated_at: meeting.updated_at.0.to_rfc3339(),
                transcripts: meeting_transcripts,
            }))
        } else {
            transaction.rollback().await?;
            Ok(None)
        }
    }

    /// Get meeting metadata without transcripts (for pagination)
    pub async fn get_meeting_metadata(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Option<MeetingModel>, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        let meeting: Option<MeetingModel> = sqlx::query_as(
            "SELECT id, title, created_at, updated_at, folder_path, pinned, archived FROM meetings WHERE id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?;

        Ok(meeting)
    }

    /// Get meeting transcripts with pagination support
    pub async fn get_meeting_transcripts_paginated(
        pool: &SqlitePool,
        meeting_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Transcript>, i64), SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        // Get total count of transcripts for this meeting
        let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM transcripts WHERE meeting_id = ?")
            .bind(meeting_id)
            .fetch_one(pool)
            .await?;

        // Get paginated transcripts ordered by audio_start_time
        let transcripts = sqlx::query_as::<_, Transcript>(
            "SELECT * FROM transcripts
             WHERE meeting_id = ?
             ORDER BY audio_start_time ASC
             LIMIT ? OFFSET ?",
        )
        .bind(meeting_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

        Ok((transcripts, total.0))
    }

    /// The user's rename (meeting header, sidebar, command palette). It marks
    /// the title as the user's, so no automatic path changes it again.
    pub async fn update_meeting_title(
        pool: &SqlitePool,
        meeting_id: &str,
        new_title: &str,
    ) -> Result<bool, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        let now = Utc::now().naive_utc();

        let rows_affected = sqlx::query(
            "UPDATE meetings SET title = ?, title_edited = 1, updated_at = ? WHERE id = ?",
        )
        .bind(new_title)
        .bind(now)
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;
        if rows_affected.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(false);
        }
        transaction.commit().await?;
        Ok(true)
    }

    /// Saves a generated meeting title.
    ///
    /// `TitleSource::Generated` (the AI notes naming the meeting) never
    /// overwrites a title the user chose and returns `TitleUpdate::Kept`
    /// instead. `TitleSource::Requested` ("Regenerate title") always saves,
    /// and counts as the user choosing that title, so later automatic passes
    /// keep it too.
    pub async fn update_meeting_name(
        pool: &SqlitePool,
        meeting_id: &str,
        new_title: &str,
        source: TitleSource,
    ) -> Result<TitleUpdate, SqlxError> {
        let mut transaction = pool.begin().await?;
        let now = Utc::now();

        let query = match source {
            TitleSource::Generated => {
                "UPDATE meetings SET title = ?, updated_at = ? WHERE id = ? AND title_edited = 0"
            }
            TitleSource::Requested => {
                "UPDATE meetings SET title = ?, title_edited = 1, updated_at = ? WHERE id = ?"
            }
        };
        let meeting_update = sqlx::query(query)
            .bind(new_title)
            .bind(now)
            .bind(meeting_id)
            .execute(&mut *transaction)
            .await?;

        if meeting_update.rows_affected() == 0 {
            let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
                .bind(meeting_id)
                .fetch_optional(&mut *transaction)
                .await?;
            transaction.rollback().await?;
            return Ok(if exists.is_some() {
                TitleUpdate::Kept
            } else {
                TitleUpdate::NotFound
            });
        }

        // Update transcript_chunks table
        sqlx::query("UPDATE transcript_chunks SET meeting_name = ? WHERE meeting_id = ?")
            .bind(new_title)
            .bind(meeting_id)
            .execute(&mut *transaction)
            .await?;

        transaction.commit().await?;
        Ok(TitleUpdate::Saved)
    }

    /// Pin (favorite) or unpin a meeting.
    pub async fn set_meeting_pinned(
        pool: &SqlitePool,
        meeting_id: &str,
        pinned: bool,
    ) -> Result<bool, SqlxError> {
        Self::set_meeting_flag(pool, meeting_id, "pinned", pinned).await
    }

    /// Archive or unarchive a meeting.
    pub async fn set_meeting_archived(
        pool: &SqlitePool,
        meeting_id: &str,
        archived: bool,
    ) -> Result<bool, SqlxError> {
        Self::set_meeting_flag(pool, meeting_id, "archived", archived).await
    }

    /// Mark a meeting as debug, or keep it as a real meeting.
    pub async fn set_meeting_debug(
        pool: &SqlitePool,
        meeting_id: &str,
        is_debug: bool,
    ) -> Result<bool, SqlxError> {
        Self::set_meeting_flag(pool, meeting_id, "is_debug", is_debug).await
    }

    /// Shared implementation for boolean meeting flags.
    ///
    /// `column` is always one of this module's own constants, never user input.
    async fn set_meeting_flag(
        pool: &SqlitePool,
        meeting_id: &str,
        column: &str,
        value: bool,
    ) -> Result<bool, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        let query = format!("UPDATE meetings SET {column} = ?, updated_at = ? WHERE id = ?");
        let now = Utc::now().naive_utc();

        let result = sqlx::query(&query)
            .bind(value as i64)
            .bind(now)
            .bind(meeting_id)
            .execute(pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}

/// Every table holding a meeting's data, keyed by `meeting_id`, in the order
/// `delete_meeting` clears them. A new meeting-keyed table belongs here; the
/// `every_meeting_keyed_table_is_listed_for_delete` test (api.rs) fails until it is.
pub(crate) const MEETING_TABLES: &[&str] = &[
    // Transcript edits and speaker fixes point at transcript rows.
    "transcript_revisions",
    "speaker_segment_overrides",
    "speaker_identities",
    "transcripts",
    "transcript_chunks",
    // Summary processes (and the summary model provenance they record).
    "summary_processes",
    // Notes: the live notes / editor document.
    "meeting_notes",
    // Live summary timeline and the enhanced notes sections.
    "meeting_timeline_entries",
    "meeting_enhanced_sections",
    // The AI chat thread, including live Copilot moments.
    "meeting_chat_messages",
    // Speaker identification runs (the diarization model provenance) and state.
    "diarization_runs",
    "diarization_state",
    // A queued or waiting final pass, so it is not resumed for a gone meeting.
    "pending_final_passes",
    // Webhook deliveries still carrying the meeting's content.
    "webhook_deliveries",
    // Who attended (the people themselves outlive the meeting).
    "meeting_attendees",
];

async fn delete_meeting_with_transaction(
    transaction: &mut SqliteConnection,
    meeting_id: &str,
) -> Result<bool, SqlxError> {
    // Check if meeting exists
    let meeting_exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(&mut *transaction)
        .await?;

    if meeting_exists.is_none() {
        error!("Meeting {} not found for deletion", meeting_id);
        return Ok(false);
    }

    // Every table keyed by meeting_id. Their foreign keys cascade too, but
    // deleting explicitly keeps this independent of the foreign_keys pragma.
    // Rows that reference transcripts go before the transcripts.
    for table in MEETING_TABLES {
        sqlx::query(&format!("DELETE FROM {table} WHERE meeting_id = ?"))
            .bind(meeting_id)
            .execute(&mut *transaction)
            .await?;
    }

    // Finally, delete the meeting
    let result = sqlx::query("DELETE FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;

    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // api_get_meeting maps this error to "Meeting not found", which transcript
    // recovery uses to tell a deleted meeting from a failed read.
    #[tokio::test]
    async fn get_meeting_reports_a_missing_row_as_row_not_found() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE meetings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                folder_path TEXT,
                pinned INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = MeetingsRepository::get_meeting(&pool, "deleted-meeting").await;

        assert!(matches!(result, Err(SqlxError::RowNotFound)));
    }

    async fn migrated_pool() -> SqlitePool {
        // One connection: each in-memory connection is its own database.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn title_and_flag(pool: &SqlitePool, meeting_id: &str) -> (String, i64) {
        sqlx::query_as("SELECT title, title_edited FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn new_meetings_start_unedited_and_take_generated_titles() {
        let pool = migrated_pool().await;
        let id =
            MeetingsRepository::create_meeting(&pool, "Meeting 24_11_26_10_00_00", None, false)
                .await
                .unwrap();

        let update =
            MeetingsRepository::update_meeting_name(&pool, &id, "Pricing", TitleSource::Generated)
                .await
                .unwrap();

        assert_eq!(update, TitleUpdate::Saved);
        assert_eq!(title_and_flag(&pool, &id).await, ("Pricing".to_string(), 0));
    }

    #[tokio::test]
    async fn a_rename_marks_the_title_and_generated_titles_keep_it() {
        let pool = migrated_pool().await;
        let id =
            MeetingsRepository::create_meeting(&pool, "Meeting 24_11_26_10_00_00", None, false)
                .await
                .unwrap();

        assert!(
            MeetingsRepository::update_meeting_title(&pool, &id, "Board prep")
                .await
                .unwrap()
        );
        assert_eq!(
            title_and_flag(&pool, &id).await,
            ("Board prep".to_string(), 1)
        );

        let update = MeetingsRepository::update_meeting_name(
            &pool,
            &id,
            "Roadmap Sync",
            TitleSource::Generated,
        )
        .await
        .unwrap();

        assert_eq!(update, TitleUpdate::Kept);
        assert_eq!(
            title_and_flag(&pool, &id).await,
            ("Board prep".to_string(), 1)
        );
    }

    #[tokio::test]
    async fn regenerate_title_replaces_a_rename_and_stays_chosen() {
        let pool = migrated_pool().await;
        let id =
            MeetingsRepository::create_meeting(&pool, "Meeting 24_11_26_10_00_00", None, false)
                .await
                .unwrap();
        MeetingsRepository::update_meeting_title(&pool, &id, "Board prep")
            .await
            .unwrap();

        let regenerated = MeetingsRepository::update_meeting_name(
            &pool,
            &id,
            "Q4 budget",
            TitleSource::Requested,
        )
        .await
        .unwrap();
        assert_eq!(regenerated, TitleUpdate::Saved);
        assert_eq!(
            title_and_flag(&pool, &id).await,
            ("Q4 budget".to_string(), 1)
        );

        let later = MeetingsRepository::update_meeting_name(
            &pool,
            &id,
            "Roadmap Sync",
            TitleSource::Generated,
        )
        .await
        .unwrap();
        assert_eq!(later, TitleUpdate::Kept);
        assert_eq!(title_and_flag(&pool, &id).await.0, "Q4 budget");
    }

    #[tokio::test]
    async fn regenerate_title_on_an_unedited_meeting_marks_it_chosen() {
        let pool = migrated_pool().await;
        let id =
            MeetingsRepository::create_meeting(&pool, "Meeting 24_11_26_10_00_00", None, false)
                .await
                .unwrap();

        MeetingsRepository::update_meeting_name(&pool, &id, "Q4 budget", TitleSource::Requested)
            .await
            .unwrap();

        assert_eq!(
            title_and_flag(&pool, &id).await,
            ("Q4 budget".to_string(), 1)
        );
    }

    #[tokio::test]
    async fn generated_titles_for_a_missing_meeting_report_not_found() {
        let pool = migrated_pool().await;

        for source in [TitleSource::Generated, TitleSource::Requested] {
            let update = MeetingsRepository::update_meeting_name(&pool, "gone", "Pricing", source)
                .await
                .unwrap();
            assert_eq!(update, TitleUpdate::NotFound);
        }
        assert!(
            !MeetingsRepository::update_meeting_title(&pool, "gone", "Pricing")
                .await
                .unwrap()
        );
    }
}
