use crate::api::{MeetingTranscriptMatch, TranscriptSearchResult, TranscriptSegment};
use chrono::Utc;
use sqlx::{Connection, Error as SqlxError, SqlitePool};
use tracing::{error, info};
use uuid::Uuid;

pub struct TranscriptsRepository;

impl TranscriptsRepository {
    /// Saves a new meeting and its associated transcript segments.
    /// This function uses a transaction to ensure that either both the meeting
    /// and all its transcripts are saved, or none of them are.
    ///
    /// When `meeting_id` is `Some` and `append` is `true`, the existing meeting
    /// row is updated (title/folder) as usual, but its existing transcript rows
    /// are kept and the new segments are inserted alongside them. This supports
    /// resuming a previously stopped meeting, where the new segments' audio
    /// timing has already been shifted to line up with the concatenated audio.
    /// When `append` is `false` (or `meeting_id` is `None`), behavior is
    /// unchanged from before: an existing meeting's transcript rows are
    /// replaced, and a missing `meeting_id` creates a brand new meeting.
    pub async fn save_transcript(
        pool: &SqlitePool,
        meeting_title: &str,
        transcripts: &[TranscriptSegment],
        folder_path: Option<String>,
        meeting_id: Option<&str>,
        append: bool,
    ) -> Result<String, SqlxError> {
        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        let now = Utc::now();

        // When the frontend already created the meeting at recording start, update
        // that row (title/folder) and replace its transcript rows. Otherwise create
        // a brand new meeting, preserving the legacy save path.
        let meeting_id = match meeting_id {
            Some(existing_id) => {
                let updated = sqlx::query(
                    "UPDATE meetings SET title = ?, folder_path = ?, updated_at = ? WHERE id = ?",
                )
                .bind(meeting_title)
                .bind(&folder_path)
                .bind(now)
                .bind(existing_id)
                .execute(&mut *transaction)
                .await?;

                if updated.rows_affected() == 0 {
                    transaction.rollback().await?;
                    return Err(SqlxError::RowNotFound);
                }

                if append {
                    info!(
                        "Appending transcripts to existing meeting with id: {}",
                        existing_id
                    );
                } else {
                    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
                        .bind(existing_id)
                        .execute(&mut *transaction)
                        .await?;

                    info!("Updating existing meeting with id: {}", existing_id);
                }
                existing_id.to_string()
            }
            None => {
                let new_id = format!("meeting-{}", Uuid::new_v4());

                let result = sqlx::query(
                    "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, ?, ?, ?, ?)",
                )
                .bind(&new_id)
                .bind(meeting_title)
                .bind(now)
                .bind(now)
                .bind(&folder_path)
                .execute(&mut *transaction)
                .await;

                if let Err(e) = result {
                    error!("Failed to create meeting '{}': {}", meeting_title, e);
                    transaction.rollback().await?;
                    return Err(e);
                }

                info!("Successfully created meeting with id: {}", new_id);
                new_id
            }
        };

        // 2. Save each transcript segment with audio timing fields
        for segment in transcripts {
            let transcript_id = format!("transcript-{}", Uuid::new_v4());
            let result = sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, speaker, audio_start_time, audio_end_time, duration)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&transcript_id)
            .bind(&meeting_id)
            .bind(&segment.text)
            .bind(&segment.timestamp)
            .bind(&segment.speaker)
            .bind(segment.audio_start_time)
            .bind(segment.audio_end_time)
            .bind(segment.duration)
            .execute(&mut *transaction)
            .await;

            if let Err(e) = result {
                error!(
                    "Failed to save transcript segment for meeting {}: {}",
                    meeting_id, e
                );
                transaction.rollback().await?;
                return Err(e);
            }
        }

        info!(
            "Successfully saved {} transcript segments for meeting {}",
            transcripts.len(),
            meeting_id
        );

        // Commit the transaction
        transaction.commit().await?;

        Ok(meeting_id)
    }

    /// Searches for a query string within the transcripts.
    /// It returns a list of matching transcripts with context.
    pub async fn search_transcripts(
        pool: &SqlitePool,
        query: &str,
    ) -> Result<Vec<TranscriptSearchResult>, SqlxError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let search_query = format!("%{}%", query.to_lowercase());

        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT m.id, m.title, t.transcript, t.timestamp
             FROM meetings m
             JOIN transcripts t ON m.id = t.meeting_id
             WHERE LOWER(t.transcript) LIKE ?",
        )
        .bind(&search_query)
        .fetch_all(pool)
        .await?;

        let results = rows
            .into_iter()
            .map(|(id, title, transcript, timestamp)| {
                let match_context = Self::get_match_context(&transcript, query);
                TranscriptSearchResult {
                    id,
                    title,
                    match_context,
                    timestamp,
                }
            })
            .collect();

        Ok(results)
    }

    /// The rows of one meeting whose text contains `query` (case-insensitive,
    /// taken literally: `%` and `_` are not wildcards), ordered like the
    /// paginated transcript (by audio start time).
    pub async fn search_meeting_transcript(
        pool: &SqlitePool,
        meeting_id: &str,
        query: &str,
    ) -> Result<Vec<MeetingTranscriptMatch>, SqlxError> {
        let query = query.trim();
        if query.is_empty() || meeting_id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let escaped = query
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{}%", escaped);

        let rows = sqlx::query_as::<_, (String, Option<f64>)>(
            "SELECT id, audio_start_time
             FROM transcripts
             WHERE meeting_id = ? AND LOWER(transcript) LIKE ? ESCAPE '\\'
             ORDER BY audio_start_time ASC",
        )
        .bind(meeting_id)
        .bind(&pattern)
        .fetch_all(pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, audio_start_time)| MeetingTranscriptMatch {
                id,
                audio_start_time,
            })
            .collect())
    }

    /// Helper function to extract a snippet of text around the first match of a query.
    fn get_match_context(transcript: &str, query: &str) -> String {
        let transcript_lower = transcript.to_lowercase();
        let query_lower = query.to_lowercase();

        match transcript_lower.find(&query_lower) {
            Some(match_index) => {
                let start_index = match_index.saturating_sub(100);
                let end_index = (match_index + query.len() + 100).min(transcript.len());

                let mut context = String::new();
                if start_index > 0 {
                    context.push_str("...");
                }
                context.push_str(&transcript[start_index..end_index]);
                if end_index < transcript.len() {
                    context.push_str("...");
                }
                context
            }
            None => transcript.chars().take(200).collect(), // Fallback to the start of the transcript
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE meetings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                folder_path TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE transcripts (
                id TEXT PRIMARY KEY,
                meeting_id TEXT NOT NULL,
                transcript TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                speaker TEXT,
                audio_start_time REAL,
                audio_end_time REAL,
                duration REAL,
                FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    fn segment(text: &str, start: f64, end: f64) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("seg-{}", Uuid::new_v4()),
            text: text.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            speaker: Some("mic".to_string()),
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            duration: Some(end - start),
        }
    }

    async fn transcript_count(pool: &SqlitePool, meeting_id: &str) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM transcripts WHERE meeting_id = ?")
            .bind(meeting_id)
            .fetch_one(pool)
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn append_keeps_old_rows_and_adds_new_ones() {
        let pool = test_pool().await;

        let first_batch = [segment("hello", 0.0, 1.0)];
        let meeting_id = TranscriptsRepository::save_transcript(
            &pool,
            "Standup",
            &first_batch,
            None,
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(transcript_count(&pool, &meeting_id).await, 1);

        let second_batch = [segment("world", 10.0, 11.0)];
        let resumed_id = TranscriptsRepository::save_transcript(
            &pool,
            "Standup",
            &second_batch,
            None,
            Some(&meeting_id),
            true,
        )
        .await
        .unwrap();

        assert_eq!(resumed_id, meeting_id);
        assert_eq!(transcript_count(&pool, &meeting_id).await, 2);
    }

    #[tokio::test]
    async fn non_append_replaces_existing_rows() {
        let pool = test_pool().await;

        let first_batch = [segment("hello", 0.0, 1.0), segment("there", 1.0, 2.0)];
        let meeting_id = TranscriptsRepository::save_transcript(
            &pool,
            "Standup",
            &first_batch,
            None,
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(transcript_count(&pool, &meeting_id).await, 2);

        let second_batch = [segment("world", 10.0, 11.0)];
        TranscriptsRepository::save_transcript(
            &pool,
            "Standup",
            &second_batch,
            None,
            Some(&meeting_id),
            false,
        )
        .await
        .unwrap();

        assert_eq!(transcript_count(&pool, &meeting_id).await, 1);
    }

    /// The text of each matched row, looked up by the id the search returned.
    async fn matched_texts(pool: &SqlitePool, matches: &[MeetingTranscriptMatch]) -> Vec<String> {
        let mut texts = Vec::new();
        for found in matches {
            let (text,) =
                sqlx::query_as::<_, (String,)>("SELECT transcript FROM transcripts WHERE id = ?")
                    .bind(&found.id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            texts.push(text);
        }
        texts
    }

    #[tokio::test]
    async fn meeting_search_covers_every_row_in_transcript_order() {
        let pool = test_pool().await;
        // More rows than one page of the panel, with the matches far apart.
        let mut segments: Vec<TranscriptSegment> = (0..250)
            .map(|index| {
                segment(
                    &format!("filler line {}", index),
                    index as f64,
                    index as f64 + 0.5,
                )
            })
            .collect();
        segments[3].text = "Budget review".to_string();
        segments[240].text = "the BUDGET is final".to_string();
        let meeting_id =
            TranscriptsRepository::save_transcript(&pool, "Planning", &segments, None, None, false)
                .await
                .unwrap();
        TranscriptsRepository::save_transcript(
            &pool,
            "Other",
            &[segment("budget elsewhere", 0.0, 1.0)],
            None,
            None,
            false,
        )
        .await
        .unwrap();

        let matches =
            TranscriptsRepository::search_meeting_transcript(&pool, &meeting_id, " budget ")
                .await
                .unwrap();
        assert_eq!(
            matches
                .iter()
                .map(|found| found.audio_start_time)
                .collect::<Vec<_>>(),
            vec![Some(3.0), Some(240.0)]
        );
        assert_eq!(
            matched_texts(&pool, &matches).await,
            vec![
                "Budget review".to_string(),
                "the BUDGET is final".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn meeting_search_takes_wildcards_literally_and_ignores_blank_queries() {
        let pool = test_pool().await;
        let segments = [
            segment("50% done", 0.0, 1.0),
            segment("fully done", 1.0, 2.0),
            segment("snake_case", 2.0, 3.0),
            segment("snakeXcase", 3.0, 4.0),
        ];
        let meeting_id = TranscriptsRepository::save_transcript(
            &pool,
            "Wildcards",
            &segments,
            None,
            None,
            false,
        )
        .await
        .unwrap();

        let percent = TranscriptsRepository::search_meeting_transcript(&pool, &meeting_id, "0%")
            .await
            .unwrap();
        assert_eq!(
            matched_texts(&pool, &percent).await,
            vec!["50% done".to_string()]
        );
        let underscore =
            TranscriptsRepository::search_meeting_transcript(&pool, &meeting_id, "e_c")
                .await
                .unwrap();
        assert_eq!(
            matched_texts(&pool, &underscore).await,
            vec!["snake_case".to_string()]
        );
        assert!(
            TranscriptsRepository::search_meeting_transcript(&pool, &meeting_id, "   ")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
