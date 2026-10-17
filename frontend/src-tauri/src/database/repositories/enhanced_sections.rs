//! Enhanced notes sections written during a meeting
//! (`meeting_enhanced_sections`): one per live-notes topic run, keyed by the
//! run's start. Writing a section again (Re-enhance) replaces it.

use serde::{Deserialize, Serialize};
use sqlx::{Error as SqlxError, FromRow, SqlitePool};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FromRow)]
pub struct EnhancedSectionRow {
    pub meeting_id: String,
    /// Audio time from the recording start, in milliseconds.
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
    /// The bullets, Markdown.
    pub body: String,
    /// Open questions, one per line (each ending in its link).
    pub questions: String,
    pub provider: String,
    pub model: String,
    pub created_at: String,
}

pub struct EnhancedSectionsRepository;

impl EnhancedSectionsRepository {
    /// Store a section, replacing one written earlier for the same run.
    /// Fails when the meeting no longer exists (the foreign key).
    pub async fn upsert(pool: &SqlitePool, row: &EnhancedSectionRow) -> Result<(), SqlxError> {
        sqlx::query(
            "INSERT INTO meeting_enhanced_sections
                (meeting_id, start_ms, end_ms, title, body, questions, provider, model, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(meeting_id, start_ms) DO UPDATE SET
                end_ms = excluded.end_ms,
                title = excluded.title,
                body = excluded.body,
                questions = excluded.questions,
                provider = excluded.provider,
                model = excluded.model,
                created_at = excluded.created_at",
        )
        .bind(&row.meeting_id)
        .bind(row.start_ms)
        .bind(row.end_ms)
        .bind(&row.title)
        .bind(&row.body)
        .bind(&row.questions)
        .bind(&row.provider)
        .bind(&row.model)
        .bind(&row.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn list_for_meeting(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Vec<EnhancedSectionRow>, SqlxError> {
        sqlx::query_as::<_, EnhancedSectionRow>(
            "SELECT meeting_id, start_ms, end_ms, title, body, questions, provider, model, created_at
             FROM meeting_enhanced_sections
             WHERE meeting_id = ?
             ORDER BY start_ms ASC",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::repositories::meeting::MeetingsRepository;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn row(meeting_id: &str, start_ms: i64, body: &str) -> EnhancedSectionRow {
        EnhancedSectionRow {
            meeting_id: meeting_id.to_string(),
            start_ms,
            end_ms: start_ms + 60_000,
            title: "Pricing".to_string(),
            body: body.to_string(),
            questions: String::new(),
            provider: "builtin-ai".to_string(),
            model: "gemma4:e2b".to_string(),
            created_at: "2026-10-17T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn sections_are_listed_by_time_and_rewriting_one_replaces_it() {
        let pool = pool().await;
        let meeting_id = MeetingsRepository::create_meeting(&pool, "Standup", None, false)
            .await
            .unwrap();
        for (start, body) in [(300_000, "- later"), (0, "- first"), (0, "- rewritten")] {
            EnhancedSectionsRepository::upsert(&pool, &row(&meeting_id, start, body))
                .await
                .unwrap();
        }
        let listed = EnhancedSectionsRepository::list_for_meeting(&pool, &meeting_id)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|r| r.body.as_str()).collect::<Vec<_>>(),
            ["- rewritten", "- later"]
        );
    }

    #[tokio::test]
    async fn a_deleted_meeting_gets_no_sections() {
        let pool = pool().await;
        assert!(
            EnhancedSectionsRepository::upsert(&pool, &row("meeting-gone", 0, "- x"))
                .await
                .is_err()
        );
    }
}
