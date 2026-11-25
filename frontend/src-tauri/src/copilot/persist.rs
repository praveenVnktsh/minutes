//! Copilot moments in the meeting's AI chat thread (`meeting_chat_messages`).

use sqlx::SqlitePool;

use super::core::ChatRow;

/// Inserts rows for `meeting_id`. Re-inserting the same ids is a no-op, so a
/// retry is safe. Fails (foreign key) when the meeting was deleted, which is
/// what keeps a discarded meeting's moments from coming back.
pub async fn insert_rows(
    pool: &SqlitePool,
    meeting_id: &str,
    rows: &[ChatRow],
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO meeting_chat_messages
               (id, meeting_id, role, content, created_at, kind, label, audio_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(meeting_id)
        .bind(row.role)
        .bind(&row.content)
        .bind(&row.created_at)
        .bind(row.kind)
        .bind(&row.label)
        .bind(row.audio_ms)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await
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

    fn rows(prefix: &str) -> Vec<ChatRow> {
        ["user", "assistant"]
            .into_iter()
            .enumerate()
            .map(|(i, role)| ChatRow {
                id: format!("{prefix}-{i}"),
                role,
                content: format!("{role} text"),
                created_at: format!("2026-10-16T10:00:0{i}Z"),
                kind: "live_action",
                label: Some("What should I say?".into()),
                audio_ms: 1_531_000,
            })
            .collect()
    }

    #[tokio::test]
    async fn moments_are_stored_once_with_kind_label_and_time() {
        let pool = pool().await;
        let meeting = MeetingsRepository::create_meeting(&pool, "Standup", None, false)
            .await
            .unwrap();
        insert_rows(&pool, &meeting, &rows("m")).await.unwrap();
        insert_rows(&pool, &meeting, &rows("m")).await.unwrap();
        let stored: Vec<(String, Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT role, kind, label, audio_ms FROM meeting_chat_messages WHERE meeting_id = ? ORDER BY created_at",
        )
        .bind(&meeting)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].0, "user");
        assert_eq!(stored[0].1.as_deref(), Some("live_action"));
        assert_eq!(stored[0].2.as_deref(), Some("What should I say?"));
        assert_eq!(stored[0].3, Some(1_531_000));
    }

    #[tokio::test]
    async fn a_deleted_meeting_takes_its_moments_and_refuses_new_ones() {
        let pool = pool().await;
        let meeting = MeetingsRepository::create_meeting(&pool, "Standup", None, false)
            .await
            .unwrap();
        insert_rows(&pool, &meeting, &rows("m")).await.unwrap();
        assert!(MeetingsRepository::delete_meeting(&pool, &meeting)
            .await
            .unwrap());
        let left: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meeting_chat_messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left.0, 0);
        assert!(insert_rows(&pool, &meeting, &rows("late")).await.is_err());
    }
}
