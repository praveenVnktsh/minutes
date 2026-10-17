use crate::state::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;

const NOTES_FILE: &str = "live-notes.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveNote {
    pub id: String,
    pub timestamp_seconds: f64,
    pub text: String,
    #[serde(default)]
    pub important: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveNotesDocument {
    pub version: u32,
    pub meeting_started_at_ms: i64,
    pub updated_at: String,
    pub notes: Vec<LiveNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_blocks: Option<serde_json::Value>,
}

fn notes_path(folder_path: &str) -> Result<PathBuf, String> {
    let folder = Path::new(folder_path);
    if !folder.is_dir() {
        return Err("Meeting recording folder does not exist".to_string());
    }
    Ok(folder.join(NOTES_FILE))
}

fn write_document(path: &Path, document: &LiveNotesDocument) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("Could not serialize live notes: {error}"))?;
    let temporary_path = path.with_extension("json.tmp");
    std::fs::write(&temporary_path, payload)
        .map_err(|error| format!("Could not save live notes: {error}"))?;
    std::fs::rename(&temporary_path, path)
        .map_err(|error| format!("Could not finalize live notes: {error}"))?;
    Ok(())
}

fn read_document(path: &Path) -> Result<Option<LiveNotesDocument>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let payload =
        std::fs::read(path).map_err(|error| format!("Could not read live notes: {error}"))?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|error| format!("Could not parse live notes: {error}"))
}

fn notes_markdown(document: &LiveNotesDocument) -> String {
    if let Some(markdown) = document
        .raw_markdown
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return markdown.to_string();
    }
    document
        .notes
        .iter()
        .filter(|note| !note.text.is_empty())
        .map(|note| {
            let seconds = note.timestamp_seconds.max(0.0).floor() as u64;
            let marker = if note.important {
                " **Important:**"
            } else {
                ""
            };
            format!(
                "- [{:02}:{:02}]{} {}",
                seconds / 60,
                seconds % 60,
                marker,
                note.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn store_meeting_notes(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    document: &LiveNotesDocument,
) -> Result<(), String> {
    let notes_json = serde_json::to_string(document)
        .map_err(|error| format!("Could not serialize live notes: {error}"))?;
    let markdown = notes_markdown(document);
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO meeting_notes (meeting_id, notes_markdown, notes_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(meeting_id) DO UPDATE SET
           notes_markdown = excluded.notes_markdown,
           notes_json = excluded.notes_json,
           updated_at = excluded.updated_at",
    )
    .bind(meeting_id)
    .bind(markdown)
    .bind(notes_json)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|error| format!("Could not attach notes to meeting: {error}"))?;
    Ok(())
}

#[tauri::command]
pub async fn save_live_notes(
    folder_path: String,
    document: LiveNotesDocument,
) -> Result<(), String> {
    let path = notes_path(&folder_path)?;
    tauri::async_runtime::spawn_blocking(move || write_document(&path, &document))
        .await
        .map_err(|error| format!("Live-notes save task failed: {error}"))?
}

#[tauri::command]
pub async fn load_live_notes(folder_path: String) -> Result<Option<LiveNotesDocument>, String> {
    let path = notes_path(&folder_path)?;
    tauri::async_runtime::spawn_blocking(move || read_document(&path))
        .await
        .map_err(|error| format!("Live-notes load task failed: {error}"))?
}

#[tauri::command]
pub async fn attach_live_notes(
    meeting_id: String,
    folder_path: String,
    state: State<'_, AppState>,
) -> Result<Option<LiveNotesDocument>, String> {
    let path = notes_path(&folder_path)?;
    let document = tauri::async_runtime::spawn_blocking(move || read_document(&path))
        .await
        .map_err(|error| format!("Live-notes load task failed: {error}"))??;
    let Some(document) = document else {
        return Ok(None);
    };

    store_meeting_notes(state.db_manager.pool(), &meeting_id, &document).await?;

    Ok(Some(document))
}

#[tauri::command]
pub async fn get_meeting_live_notes(
    meeting_id: String,
    state: State<'_, AppState>,
) -> Result<Option<LiveNotesDocument>, String> {
    let notes_json = sqlx::query_scalar::<_, String>(
        "SELECT notes_json FROM meeting_notes WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(state.db_manager.pool())
    .await
    .map_err(|error| format!("Could not load meeting notes: {error}"))?;

    notes_json
        .map(|json| {
            serde_json::from_str(&json)
                .map_err(|error| format!("Could not parse stored meeting notes: {error}"))
        })
        .transpose()
}

#[tauri::command]
pub async fn save_meeting_live_notes(
    meeting_id: String,
    document: LiveNotesDocument,
    state: State<'_, AppState>,
) -> Result<(), String> {
    store_meeting_notes(state.db_manager.pool(), &meeting_id, &document).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document() -> LiveNotesDocument {
        LiveNotesDocument {
            version: 1,
            meeting_started_at_ms: 1_750_000_000_000,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            raw_markdown: None,
            editor_blocks: None,
            notes: vec![LiveNote {
                id: "note-1".to_string(),
                timestamp_seconds: 65.8,
                text: "  Preserve this exactly.\nIncluding the newline.  ".to_string(),
                important: true,
            }],
        }
    }

    #[test]
    fn round_trip_preserves_original_note_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(NOTES_FILE);
        let document = sample_document();
        write_document(&path, &document).unwrap();
        assert_eq!(read_document(&path).unwrap(), Some(document));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn markdown_keeps_timestamp_and_importance_signal() {
        assert_eq!(
            notes_markdown(&sample_document()),
            "- [01:05] **Important:**   Preserve this exactly.\nIncluding the newline.  "
        );
    }

    #[tokio::test]
    async fn database_upsert_keeps_original_json_and_updates_markdown() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE meeting_notes (
                meeting_id TEXT PRIMARY KEY NOT NULL,
                notes_markdown TEXT,
                notes_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let document = sample_document();
        store_meeting_notes(&pool, "meeting-1", &document)
            .await
            .unwrap();

        let (json, markdown): (String, String) = sqlx::query_as(
            "SELECT notes_json, notes_markdown FROM meeting_notes WHERE meeting_id = ?",
        )
        .bind("meeting-1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<LiveNotesDocument>(&json).unwrap(),
            document
        );
        assert!(markdown.contains("[01:05] **Important:**"));
    }
}
