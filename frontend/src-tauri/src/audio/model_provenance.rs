//! Which models produced a meeting's transcript, speaker labels and summary.
//!
//! Nothing new is stored in the database for this. The transcription model is
//! written into the meeting folder's `metadata.json` when the transcript is
//! made; the diarization engine comes from the latest `diarization_runs` row;
//! the summary model comes from the source that `summary/service.rs` records in
//! the `english_cache` of `summary_processes.result`. Meetings made before any
//! of these existed simply report `None` for that part.

use crate::state::AppState;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};

/// Key in `metadata.json` holding the [`TranscriptionModel`].
const TRANSCRIPTION_MODEL_KEY: &str = "transcription_model";

/// Field of `summary_processes.result` that `summary/service.rs` writes the
/// summary's source (including its provider and model) under.
const ENGLISH_CACHE_FIELD: &str = "english_cache";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionModel {
    /// "whisper", "parakeet" or a cloud provider id.
    pub provider: String,
    /// e.g. "large-v3-turbo".
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiarizationModel {
    pub engine: String,
    pub segmentation_model: Option<String>,
    pub embedding_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryModel {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingModelProvenance {
    pub transcription: Option<TranscriptionModel>,
    pub diarization: Option<DiarizationModel>,
    pub summary: Option<SummaryModel>,
}

/// Merges `"transcription_model": {provider, model}` into
/// `<meeting_folder>/metadata.json`, preserving every other key, and writes it
/// atomically. A missing file is created holding just that key; a file that is
/// not a JSON object is left alone and reported as an error.
pub fn record_transcription_model(meeting_folder: &Path, model: &TranscriptionModel) -> Result<()> {
    let metadata_path = meeting_folder.join("metadata.json");
    let temp_path = meeting_folder.join(".metadata.json.tmp");

    let mut metadata = match std::fs::read_to_string(&metadata_path) {
        Ok(existing) => {
            let value: Value = serde_json::from_str(&existing)
                .with_context(|| format!("{} is not valid JSON", metadata_path.display()))?;
            if !value.is_object() {
                return Err(anyhow!("{} is not a JSON object", metadata_path.display()));
            }
            value
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Value::Object(serde_json::Map::new())
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Could not read {}", metadata_path.display()))
        }
    };

    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            TRANSCRIPTION_MODEL_KEY.to_string(),
            serde_json::to_value(model)?,
        );
    }

    std::fs::write(&temp_path, serde_json::to_string_pretty(&metadata)?)?;
    std::fs::rename(&temp_path, &metadata_path)?;
    log::info!(
        "Recorded transcription model {}/{} in {}",
        model.provider,
        model.model,
        metadata_path.display()
    );
    Ok(())
}

/// The transcription model recorded in `<meeting_folder>/metadata.json`, or
/// `None` when the file is missing, unparsable, or predates the key.
pub fn read_transcription_model(meeting_folder: &Path) -> Option<TranscriptionModel> {
    let raw = std::fs::read_to_string(meeting_folder.join("metadata.json")).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    serde_json::from_value(value.get(TRANSCRIPTION_MODEL_KEY)?.clone()).ok()
}

async fn meeting_folder(pool: &SqlitePool, meeting_id: &str) -> Result<Option<PathBuf>> {
    let folder_path =
        sqlx::query_scalar::<_, Option<String>>("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(folder_path
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from))
}

/// The engine and models of the meeting's most recent diarization run.
async fn latest_diarization_model(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<Option<DiarizationModel>> {
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT engine, segmentation_model, embedding_model FROM diarization_runs \
         WHERE meeting_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(engine, segmentation_model, embedding_model)| DiarizationModel {
            engine,
            segmentation_model: segmentation_model.filter(|model| !model.is_empty()),
            embedding_model: embedding_model.filter(|model| !model.is_empty()),
        },
    ))
}

/// The provider and model recorded in a `summary_processes.result` blob, or
/// `None` when it has no `english_cache` source (older or hand-edited summaries).
fn summary_model_from_result(raw: &str) -> Option<SummaryModel> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let source = value.get(ENGLISH_CACHE_FIELD)?.get("source")?;
    let provider = source.get("model_provider")?.as_str()?.trim();
    let model = source.get("model_name")?.as_str()?.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(SummaryModel {
        provider: provider.to_string(),
        model: model.to_string(),
    })
}

async fn summary_model(pool: &SqlitePool, meeting_id: &str) -> Result<Option<SummaryModel>> {
    let result = sqlx::query_scalar::<_, Option<String>>(
        "SELECT result FROM summary_processes WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(result.as_deref().and_then(summary_model_from_result))
}

/// Logs a failed lookup and treats it as unknown, so one broken source does not
/// hide the models the other two can still name.
fn or_unknown<T>(part: &str, meeting_id: &str, lookup: Result<Option<T>>) -> Option<T> {
    lookup.unwrap_or_else(|error| {
        log::warn!(
            "Model provenance: could not read {part} model for meeting {meeting_id}: {error:#}"
        );
        None
    })
}

async fn load_provenance(pool: &SqlitePool, meeting_id: &str) -> MeetingModelProvenance {
    let folder = or_unknown(
        "transcription",
        meeting_id,
        meeting_folder(pool, meeting_id).await,
    );
    MeetingModelProvenance {
        transcription: folder.as_deref().and_then(read_transcription_model),
        diarization: or_unknown(
            "diarization",
            meeting_id,
            latest_diarization_model(pool, meeting_id).await,
        ),
        summary: or_unknown("summary", meeting_id, summary_model(pool, meeting_id).await),
    }
}

/// The models that transcribed, diarized and summarized a meeting, each `None`
/// when it is unknown.
#[tauri::command]
pub async fn get_meeting_model_provenance<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
) -> Result<MeetingModelProvenance, String> {
    let pool = app
        .try_state::<AppState>()
        .ok_or_else(|| "App state not available".to_string())?
        .db_manager
        .pool()
        .clone();
    Ok(load_provenance(&pool, &meeting_id).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whisper() -> TranscriptionModel {
        TranscriptionModel {
            provider: "whisper".into(),
            model: "large-v3-turbo".into(),
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    async fn test_pool() -> SqlitePool {
        use sqlx::Executor;
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        pool.execute(
            "CREATE TABLE meetings (id TEXT PRIMARY KEY NOT NULL, folder_path TEXT); \
             CREATE TABLE summary_processes (meeting_id TEXT PRIMARY KEY, result TEXT);",
        )
        .await
        .unwrap();
        pool.execute(include_str!(
            "../../migrations/20260912000000_add_diarization_runs.sql"
        ))
        .await
        .unwrap();
        pool
    }

    async fn insert_run(pool: &SqlitePool, id: &str, engine: &str, created_at: &str) {
        sqlx::query(
            "INSERT INTO diarization_runs (id, meeting_id, engine, segmentation_model, embedding_model, result_json, created_at) \
             VALUES (?, 'm1', ?, 'seg', '', '{}', ?)",
        )
        .bind(id)
        .bind(engine)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn recording_keeps_the_other_metadata_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        std::fs::write(
            &path,
            r#"{"meeting_id":"m1","status":"completed","transcription_model":{"provider":"parakeet","model":"old"}}"#,
        )
        .unwrap();

        record_transcription_model(dir.path(), &whisper()).unwrap();

        let json = read_json(&path);
        assert_eq!(json["meeting_id"], "m1");
        assert_eq!(json["status"], "completed");
        assert_eq!(
            json["transcription_model"],
            serde_json::json!({"provider": "whisper", "model": "large-v3-turbo"})
        );
        assert!(!dir.path().join(".metadata.json.tmp").exists());
        assert_eq!(read_transcription_model(dir.path()), Some(whisper()));
    }

    #[test]
    fn recording_creates_a_missing_metadata_file() {
        let dir = tempfile::tempdir().unwrap();
        record_transcription_model(dir.path(), &whisper()).unwrap();
        let json = read_json(&dir.path().join("metadata.json"));
        assert_eq!(json.as_object().unwrap().len(), 1);
        assert_eq!(read_transcription_model(dir.path()), Some(whisper()));
    }

    #[test]
    fn recording_refuses_to_clobber_malformed_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        for broken in ["{not json", "[1, 2]"] {
            std::fs::write(&path, broken).unwrap();
            assert!(record_transcription_model(dir.path(), &whisper()).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        }
    }

    #[test]
    fn reading_old_or_broken_metadata_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_transcription_model(dir.path()), None);
        let path = dir.path().join("metadata.json");
        std::fs::write(&path, r#"{"meeting_id":"m1"}"#).unwrap();
        assert_eq!(read_transcription_model(dir.path()), None);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(read_transcription_model(dir.path()), None);
        std::fs::write(&path, r#"{"transcription_model":{"provider":"whisper"}}"#).unwrap();
        assert_eq!(read_transcription_model(dir.path()), None);
    }

    #[test]
    fn summary_model_comes_from_the_english_cache_source() {
        let raw = serde_json::json!({
            "markdown": "# Notes",
            "english_cache": {
                "markdown": "# Notes",
                "source": {"model_provider": "ollama", "model_name": "llama3.2:latest", "template_id": "standard"},
                "output_language": null
            }
        })
        .to_string();
        assert_eq!(
            summary_model_from_result(&raw),
            Some(SummaryModel {
                provider: "ollama".into(),
                model: "llama3.2:latest".into()
            })
        );
        assert_eq!(
            summary_model_from_result(r##"{"markdown":"# Notes"}"##),
            None
        );
        assert_eq!(summary_model_from_result("not json"), None);
        assert_eq!(
            summary_model_from_result(
                r#"{"english_cache":{"source":{"model_provider":"","model_name":"x"}}}"#
            ),
            None
        );
    }

    #[tokio::test]
    async fn provenance_reads_each_source_for_the_meeting() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        record_transcription_model(dir.path(), &whisper()).unwrap();
        sqlx::query("INSERT INTO meetings (id, folder_path) VALUES ('m1', ?), ('m2', NULL)")
            .bind(dir.path().to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        insert_run(&pool, "r1", "sherpa-onnx", "2026-09-01T00:00:00Z").await;
        insert_run(&pool, "r2", "nemotron", "2026-09-02T00:00:00Z").await;
        sqlx::query("INSERT INTO summary_processes (meeting_id, result) VALUES ('m1', ?)")
            .bind(
                r#"{"english_cache":{"source":{"model_provider":"claude","model_name":"claude-sonnet"}}}"#,
            )
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            load_provenance(&pool, "m1").await,
            MeetingModelProvenance {
                transcription: Some(whisper()),
                diarization: Some(DiarizationModel {
                    engine: "nemotron".into(),
                    segmentation_model: Some("seg".into()),
                    embedding_model: None,
                }),
                summary: Some(SummaryModel {
                    provider: "claude".into(),
                    model: "claude-sonnet".into()
                }),
            }
        );
        assert_eq!(
            load_provenance(&pool, "m2").await,
            MeetingModelProvenance::default()
        );
        assert_eq!(
            load_provenance(&pool, "missing").await,
            MeetingModelProvenance::default()
        );
    }

    #[tokio::test]
    async fn a_missing_table_degrades_to_unknown() {
        use sqlx::Executor;
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        pool.execute("CREATE TABLE meetings (id TEXT PRIMARY KEY NOT NULL, folder_path TEXT)")
            .await
            .unwrap();
        assert_eq!(
            load_provenance(&pool, "m1").await,
            MeetingModelProvenance::default()
        );
    }
}
