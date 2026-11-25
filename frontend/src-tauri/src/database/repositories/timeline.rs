//! Live summary timeline entries (`meeting_timeline_entries`): the short
//! points, decisions and actions made while a meeting records, shown as its
//! chapters afterwards.

use serde::{Deserialize, Serialize};
use sqlx::{Error as SqlxError, FromRow, SqlitePool};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FromRow)]
pub struct TimelineEntryRow {
    pub id: String,
    pub meeting_id: String,
    /// Audio time from the recording start, in milliseconds.
    pub start_ms: i64,
    pub end_ms: i64,
    /// `point`, `decision` or `action`.
    pub kind: String,
    pub text: String,
    pub speaker: Option<String>,
    pub provider: String,
    pub model: String,
    pub created_at: String,
    /// The section title the entry sits under; NULL on entries made before
    /// topics existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

pub struct TimelineRepository;

impl TimelineRepository {
    /// Insert entries, skipping any id already stored, so a retried write is
    /// harmless. Fails (and inserts nothing) when the meeting no longer
    /// exists: the foreign key keeps a deleted meeting from coming back.
    pub async fn insert_entries(
        pool: &SqlitePool,
        entries: &[TimelineEntryRow],
    ) -> Result<u64, SqlxError> {
        if entries.is_empty() {
            return Ok(0);
        }
        let mut transaction = pool.begin().await?;
        let mut inserted = 0;
        for entry in entries {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO meeting_timeline_entries
                    (id, meeting_id, start_ms, end_ms, kind, text, speaker, provider, model, created_at, topic)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&entry.id)
            .bind(&entry.meeting_id)
            .bind(entry.start_ms)
            .bind(entry.end_ms)
            .bind(&entry.kind)
            .bind(&entry.text)
            .bind(&entry.speaker)
            .bind(&entry.provider)
            .bind(&entry.model)
            .bind(&entry.created_at)
            .bind(&entry.topic)
            .execute(&mut *transaction)
            .await?;
            inserted += result.rows_affected();
        }
        transaction.commit().await?;
        Ok(inserted)
    }

    pub async fn list_for_meeting(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Vec<TimelineEntryRow>, SqlxError> {
        sqlx::query_as::<_, TimelineEntryRow>(
            "SELECT id, meeting_id, start_ms, end_ms, kind, text, speaker, provider, model, created_at, topic
             FROM meeting_timeline_entries
             WHERE meeting_id = ?
             ORDER BY start_ms ASC, created_at ASC",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await
    }

    pub async fn delete_for_meeting(pool: &SqlitePool, meeting_id: &str) -> Result<u64, SqlxError> {
        let result = sqlx::query("DELETE FROM meeting_timeline_entries WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::repositories::meeting::MeetingsRepository;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        // One connection: each in-memory connection is its own database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn meeting(pool: &SqlitePool) -> String {
        MeetingsRepository::create_meeting(pool, "Standup", None, false)
            .await
            .unwrap()
    }

    fn entry(meeting_id: &str, id: &str, start_ms: i64, kind: &str) -> TimelineEntryRow {
        TimelineEntryRow {
            id: id.to_string(),
            meeting_id: meeting_id.to_string(),
            start_ms,
            end_ms: start_ms + 30_000,
            kind: kind.to_string(),
            text: format!("entry {id}"),
            speaker: Some("Others".to_string()),
            provider: "builtin-ai".to_string(),
            model: "qwen".to_string(),
            created_at: "2026-10-16T00:00:00Z".to_string(),
            topic: Some("Launch Plan".to_string()),
        }
    }

    #[tokio::test]
    async fn topics_are_stored_and_older_entries_without_one_still_load() {
        let pool = pool().await;
        let meeting_id = meeting(&pool).await;
        // An entry written before the topic column existed.
        sqlx::query(
            "INSERT INTO meeting_timeline_entries
                (id, meeting_id, start_ms, end_ms, kind, text, speaker, provider, model, created_at)
             VALUES ('old', ?, 0, 1000, 'point', 'Older entry', 'Others', 'builtin-ai', 'qwen', '2026-10-15T00:00:00Z')",
        )
        .bind(&meeting_id)
        .execute(&pool)
        .await
        .unwrap();
        TimelineRepository::insert_entries(&pool, &[entry(&meeting_id, "new", 5_000, "point")])
            .await
            .unwrap();

        let listed = TimelineRepository::list_for_meeting(&pool, &meeting_id)
            .await
            .unwrap();
        assert_eq!(listed[0].id, "old");
        assert_eq!(listed[0].topic, None);
        assert_eq!(listed[1].topic.as_deref(), Some("Launch Plan"));
        // An entry without a topic serializes without the field, as before.
        let json = serde_json::to_value(&listed[0]).unwrap();
        assert!(json.get("topic").is_none());
        assert_eq!(
            serde_json::to_value(&listed[1]).unwrap()["topic"],
            "Launch Plan"
        );
    }

    #[tokio::test]
    async fn stores_and_lists_entries_in_time_order() {
        let pool = pool().await;
        let meeting_id = meeting(&pool).await;
        let entries = [
            entry(&meeting_id, "b", 90_000, "decision"),
            entry(&meeting_id, "a", 10_000, "point"),
        ];
        assert_eq!(
            TimelineRepository::insert_entries(&pool, &entries)
                .await
                .unwrap(),
            2
        );

        let listed = TimelineRepository::list_for_meeting(&pool, &meeting_id)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(listed[1], entries[0]);
    }

    #[tokio::test]
    async fn reinserting_the_same_entries_is_a_no_op() {
        let pool = pool().await;
        let meeting_id = meeting(&pool).await;
        let entries = [entry(&meeting_id, "a", 0, "point")];
        TimelineRepository::insert_entries(&pool, &entries)
            .await
            .unwrap();
        assert_eq!(
            TimelineRepository::insert_entries(&pool, &entries)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            TimelineRepository::list_for_meeting(&pool, &meeting_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn entries_for_a_missing_meeting_are_rejected() {
        let pool = pool().await;
        let result =
            TimelineRepository::insert_entries(&pool, &[entry("meeting-gone", "a", 0, "point")])
                .await;
        assert!(
            result.is_err(),
            "a deleted meeting must not get entries back"
        );
    }

    #[tokio::test]
    async fn unknown_kinds_are_not_stored() {
        // INSERT OR IGNORE skips CHECK violations (but not foreign keys).
        let pool = pool().await;
        let meeting_id = meeting(&pool).await;
        let inserted =
            TimelineRepository::insert_entries(&pool, &[entry(&meeting_id, "a", 0, "chapter")])
                .await
                .unwrap();
        assert_eq!(inserted, 0);
        assert!(TimelineRepository::list_for_meeting(&pool, &meeting_id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn deleting_the_meeting_deletes_its_entries() {
        let pool = pool().await;
        let kept = meeting(&pool).await;
        let deleted = meeting(&pool).await;
        TimelineRepository::insert_entries(
            &pool,
            &[
                entry(&kept, "k", 0, "point"),
                entry(&deleted, "d", 0, "action"),
            ],
        )
        .await
        .unwrap();

        assert!(MeetingsRepository::delete_meeting(&pool, &deleted)
            .await
            .unwrap());

        assert!(TimelineRepository::list_for_meeting(&pool, &deleted)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            TimelineRepository::list_for_meeting(&pool, &kept)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn debug_meeting_cleanup_cascades_to_entries() {
        let pool = pool().await;
        let debug = MeetingsRepository::create_meeting(&pool, "Debug", None, true)
            .await
            .unwrap();
        TimelineRepository::insert_entries(&pool, &[entry(&debug, "d", 0, "point")])
            .await
            .unwrap();
        MeetingsRepository::delete_debug_meetings(&pool)
            .await
            .unwrap();
        assert!(TimelineRepository::list_for_meeting(&pool, &debug)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn delete_for_meeting_removes_only_that_meeting() {
        let pool = pool().await;
        let a = meeting(&pool).await;
        let b = meeting(&pool).await;
        TimelineRepository::insert_entries(
            &pool,
            &[entry(&a, "a", 0, "point"), entry(&b, "b", 0, "point")],
        )
        .await
        .unwrap();
        assert_eq!(
            TimelineRepository::delete_for_meeting(&pool, &a)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            TimelineRepository::list_for_meeting(&pool, &b)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
