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
    /// Epoch milliseconds. The webview computes it from a fractional
    /// duration, so a decimal is accepted and rounded rather than failing
    /// every save.
    #[serde(deserialize_with = "whole_ms")]
    pub meeting_started_at_ms: i64,
    pub updated_at: String,
    pub notes: Vec<LiveNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_blocks: Option<serde_json::Value>,
    /// Blocks the AI wrote into the notes ("Write live notes into my
    /// notes"), by block id: `{ entryId, text }`. A block the user edits
    /// leaves the map and becomes theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_blocks: Option<serde_json::Value>,
    /// Live notes already written into the notes (entry ids).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_entries: Option<serde_json::Value>,
}

/// An integer, or a finite number rounded to the nearest integer.
fn whole_ms<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    if !value.is_finite() {
        return Err(serde::de::Error::custom("meetingStartedAtMs must be a finite number"));
    }
    Ok(value.round() as i64)
}

impl LiveNotesDocument {
    /// Whether block `id` is AI text, not the user's.
    pub fn is_ai_block(&self, id: &str) -> bool {
        self.ai_blocks
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .is_some_and(|blocks| blocks.contains_key(id))
    }

    /// The user's own notes, with the audio time each was typed at; AI
    /// lines are left out.
    pub fn user_notes(&self) -> Vec<crate::summary::live_outline::UserNote> {
        self.notes
            .iter()
            .filter(|note| !note.text.trim().is_empty() && !self.is_ai_block(&note.id))
            .map(|note| crate::summary::live_outline::UserNote {
                ms: Some((note.timestamp_seconds.max(0.0) * 1000.0).round() as i64),
                text: note.text.trim().to_string(),
            })
            .collect()
    }
}

/// The live notes document of a recording folder, if there is one.
pub(crate) fn read_folder_document(folder_path: &str) -> Result<Option<LiveNotesDocument>, String> {
    read_document(&notes_path(folder_path)?)
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
        .filter(|note| !note.text.is_empty() && !document.is_ai_block(&note.id))
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
mod whole_ms_tests {
    use super::LiveNotesDocument;

    fn doc(started: &str) -> serde_json::Result<LiveNotesDocument> {
        serde_json::from_str(&format!(
            r#"{{"version":1,"meetingStartedAtMs":{started},"updatedAt":"2026-10-17T00:00:00Z","notes":[]}}"#
        ))
    }

    #[test]
    fn a_fractional_start_time_is_rounded_not_rejected() {
        assert_eq!(doc("1792265893512.629").unwrap().meeting_started_at_ms, 1792265893513);
        assert_eq!(doc("1792265893512").unwrap().meeting_started_at_ms, 1792265893512);
    }
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
            ai_blocks: None,
            ai_entries: None,
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
    fn ai_lines_are_not_the_users_notes() {
        let mut document = sample_document();
        document.notes.push(LiveNote {
            id: "ai-1".to_string(),
            timestamp_seconds: 90.0,
            text: "Pricing at $40 per seat".to_string(),
            important: false,
        });
        document.ai_blocks = Some(serde_json::json!({
            "ai-1": { "entryId": "e1", "text": "Pricing at $40 per seat" }
        }));
        let notes = document.user_notes();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].ms, Some(65_800));
        assert!(!notes_markdown(&document).contains("Pricing"));
        // The provenance survives a save and a load.
        let round_trip: LiveNotesDocument =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(round_trip.is_ai_block("ai-1"));
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
