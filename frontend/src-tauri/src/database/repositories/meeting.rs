use crate::api::{MeetingDetails, MeetingTranscript};
use crate::database::models::{MeetingModel, Transcript};
use chrono::Utc;
use sqlx::{Connection, Error as SqlxError, SqliteConnection, SqlitePool};
use tracing::{error, info};
use uuid::Uuid;

pub struct MeetingsRepository;

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

        let rows_affected =
            sqlx::query("UPDATE meetings SET title = ?, updated_at = ? WHERE id = ?")
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

    pub async fn update_meeting_name(
        pool: &SqlitePool,
        meeting_id: &str,
        new_title: &str,
    ) -> Result<bool, SqlxError> {
        let mut transaction = pool.begin().await?;
        let now = Utc::now();

        // Update meetings table
        let meeting_update =
            sqlx::query("UPDATE meetings SET title = ?, updated_at = ? WHERE id = ?")
                .bind(new_title)
                .bind(now)
                .bind(meeting_id)
                .execute(&mut *transaction)
                .await?;

        if meeting_update.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(false); // Meeting not found
        }

        // Update transcript_chunks table
        sqlx::query("UPDATE transcript_chunks SET meeting_name = ? WHERE meeting_id = ?")
            .bind(new_title)
            .bind(meeting_id)
            .execute(&mut *transaction)
            .await?;

        transaction.commit().await?;
        Ok(true)
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

    // Delete from related tables in proper order
    // 1. Delete from transcript_chunks
    sqlx::query("DELETE FROM transcript_chunks WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;

    // 2. Delete from summary_processes
    sqlx::query("DELETE FROM summary_processes WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;

    // 3. Delete from transcripts
    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;

    // 4. Finally, delete the meeting
    let result = sqlx::query("DELETE FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .execute(&mut *transaction)
        .await?;

    Ok(result.rows_affected() > 0)
}
