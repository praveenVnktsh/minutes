use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{FromRow, SqlitePool};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;
use url::Url;
use uuid::Uuid;

use crate::audio::model_provenance::read_transcription_model;
use crate::database::models::MeetingModel;
use crate::database::repositories::meeting::MeetingsRepository;
use crate::state::AppState;

const STORE_FILE: &str = "webhook-settings.json";
const STORE_KEY: &str = "transcription_complete";
const EVENT_TYPE: &str = "transcription.completed";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub signing_secret: String,
}

#[derive(Debug, Clone, Serialize)]
struct WebhookPayload {
    id: String,
    event_type: &'static str,
    created_at: String,
    meeting: MeetingPayload,
    result: ResultPayload,
}

#[derive(Debug, Clone, Serialize)]
struct MeetingPayload {
    id: String,
    title: String,
    started_at: String,
    ended_at: Option<String>,
    calendar_event_id: Option<String>,
    external_ledger_id: Option<String>,
}

/// The receiver runs on another machine and cannot read this database, so the
/// transcript itself travels in the payload: without it a delivery records that
/// a meeting happened and nothing that was said.
#[derive(Debug, Clone, Serialize)]
struct ResultPayload {
    status: &'static str,
    transcript: String,
    transcript_segments: i64,
    duration_seconds: f64,
    transcription_model: Option<String>,
    meetily_path: String,
}

#[derive(Debug, Clone, PartialEq)]
struct TranscriptReading {
    text: String,
    segments: i64,
    duration_seconds: f64,
}

/// Every segment of a meeting in the order it was spoken, one per line.
///
/// Blank segments are dropped rather than kept as empty lines, matching how
/// murmr reads a meeting's `transcripts.json`, so the two readings of one
/// meeting produce the same text.
async fn read_transcript(pool: &SqlitePool, meeting_id: &str) -> Result<TranscriptReading, sqlx::Error> {
    let rows: Vec<(String, Option<f64>)> = sqlx::query_as(
        "SELECT transcript, audio_end_time FROM transcripts
         WHERE meeting_id = ?
         ORDER BY audio_start_time IS NULL, audio_start_time, timestamp, id",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;

    let duration_seconds = rows
        .iter()
        .filter_map(|(_, end)| *end)
        .fold(0.0_f64, f64::max);
    let text = rows
        .iter()
        .map(|(text, _)| text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(TranscriptReading {
        text,
        segments: rows.len() as i64,
        duration_seconds,
    })
}

/// When the meeting ended and which engine transcribed it, from the meeting
/// folder's `metadata.json` -- the record written for this meeting, not the
/// app's current settings. A meeting retranscribed or imported with another
/// engine than the one now configured would otherwise be stamped with the
/// wrong model, and the receiver replaces its earlier reading of a meeting
/// with the latest one, so a wrong attestation would overwrite a right one.
///
/// `completed_at` is the wall-clock moment recording stopped: a resume clears
/// it and the final stop sets it again, and a retranscription keeps it. Each
/// field is `None` when the folder does not say, rather than guessed.
#[derive(Debug, Clone, Default, PartialEq)]
struct FolderFacts {
    ended_at: Option<String>,
    transcription_model: Option<String>,
}

fn folder_facts(folder: Option<&Path>, started_at: DateTime<Utc>) -> FolderFacts {
    let Some(folder) = folder else {
        return FolderFacts::default();
    };
    let ended_at = std::fs::read_to_string(folder.join("metadata.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|metadata| metadata.get("completed_at")?.as_str().map(str::to_owned))
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc))
        // The receiver rejects an end before the start; send none instead.
        .filter(|ended| *ended >= started_at)
        .map(|ended| ended.to_rfc3339());
    let transcription_model = read_transcription_model(folder)
        .map(|model| format!("{}:{}", model.provider, model.model));
    FolderFacts {
        ended_at,
        transcription_model,
    }
}

/// The body of one `transcription.completed` delivery for `meeting`.
async fn build_payload(
    pool: &SqlitePool,
    meeting: MeetingModel,
    event_id: &str,
    now: &str,
) -> Result<String, String> {
    let transcript = read_transcript(pool, &meeting.id)
        .await
        .map_err(|error| format!("Failed to load transcript for webhook: {error}"))?;
    let started_at = meeting.created_at.0;
    let folder = meeting
        .folder_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from);
    let facts = folder_facts(folder.as_deref(), started_at);
    let meetily_path = format!("/meeting-details?id={}", meeting.id);
    serde_json::to_string(&WebhookPayload {
        id: event_id.to_string(),
        event_type: EVENT_TYPE,
        created_at: now.to_string(),
        meeting: MeetingPayload {
            id: meeting.id,
            title: meeting.title,
            started_at: started_at.to_rfc3339(),
            ended_at: facts.ended_at,
            calendar_event_id: None,
            external_ledger_id: None,
        },
        result: ResultPayload {
            status: "complete",
            transcript: transcript.text,
            transcript_segments: transcript.segments,
            duration_seconds: transcript.duration_seconds,
            transcription_model: facts.transcription_model,
            meetily_path,
        },
    })
    .map_err(|error| format!("Failed to serialize webhook payload: {error}"))
}

#[derive(Debug, FromRow)]
struct PendingDelivery {
    event_id: String,
    payload: String,
    attempt_count: i64,
}

fn load_config<R: Runtime>(app: &AppHandle<R>) -> Result<WebhookConfig, String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|error| format!("Failed to open webhook settings: {error}"))?;

    match store.get(STORE_KEY) {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| format!("Failed to read webhook settings: {error}")),
        None => Ok(WebhookConfig::default()),
    }
}

fn validate_config(config: &WebhookConfig) -> Result<(), String> {
    if !config.enabled {
        return Ok(());
    }

    let endpoint = Url::parse(config.endpoint.trim())
        .map_err(|_| "Webhook endpoint must be a valid URL".to_string())?;
    let is_localhost = endpoint.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    });
    if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && is_localhost) {
        return Err("Webhook endpoint must use HTTPS (HTTP is allowed for localhost)".to_string());
    }
    if config.signing_secret.trim().len() < 16 {
        return Err("Webhook signing secret must contain at least 16 characters".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn get_webhook_config<R: Runtime>(app: AppHandle<R>) -> Result<WebhookConfig, String> {
    load_config(&app)
}

#[tauri::command]
pub async fn set_webhook_config<R: Runtime>(
    app: AppHandle<R>,
    config: WebhookConfig,
) -> Result<(), String> {
    validate_config(&config)?;
    let store = app
        .store(STORE_FILE)
        .map_err(|error| format!("Failed to open webhook settings: {error}"))?;
    store.set(
        STORE_KEY,
        serde_json::to_value(config)
            .map_err(|error| format!("Failed to serialize webhook settings: {error}"))?,
    );
    store
        .save()
        .map_err(|error| format!("Failed to save webhook settings: {error}"))
}

fn sign_payload(secret: &str, payload: &str) -> Result<String, String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| "Invalid webhook signing secret".to_string())?;
    mac.update(payload.as_bytes());
    Ok(format!(
        "sha256={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

async fn post_payload(
    config: &WebhookConfig,
    event_id: &str,
    event_type: &str,
    payload: &str,
) -> Result<(), String> {
    validate_config(config)?;
    let signature = sign_payload(&config.signing_secret, payload)?;
    let response = reqwest::Client::new()
        .post(config.endpoint.trim())
        .header("content-type", "application/json")
        .header("x-meetily-event-id", event_id)
        .header("x-meetily-event-type", event_type)
        .header("x-meetily-signature", signature)
        .body(payload.to_owned())
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| format!("Webhook request failed: {error}"))?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "Webhook endpoint returned HTTP {}",
            response.status()
        ))
    }
}

#[tauri::command]
pub async fn test_webhook<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let config = load_config(&app)?;
    let event_id = format!("evt_{}", Uuid::new_v4());
    let payload = serde_json::json!({
        "id": event_id,
        "event_type": "transcription.completed.test",
        "created_at": Utc::now().to_rfc3339(),
        "meeting": {
            "id": "test-meeting",
            "title": "Minutes webhook test",
            "started_at": Utc::now().to_rfc3339(),
            "ended_at": null,
            "calendar_event_id": null,
            "external_ledger_id": null
        },
        "result": {
            "status": "complete",
            "transcript": "",
            "transcript_segments": 1,
            "duration_seconds": 1.0,
            "transcription_model": null,
            "meetily_path": "/meeting-details?id=test-meeting"
        }
    })
    .to_string();
    post_payload(&config, &event_id, "transcription.completed.test", &payload).await
}

pub async fn enqueue_transcription_complete<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
) -> Result<bool, String> {
    let config = load_config(app)?;
    if !config.enabled {
        return Ok(false);
    }
    validate_config(&config)?;

    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "Database is not initialized".to_string())?;
    let pool = state.db_manager.pool();
    let meeting = MeetingsRepository::get_meeting_metadata(pool, meeting_id)
        .await
        .map_err(|error| format!("Failed to load meeting for webhook: {error}"))?
        .ok_or_else(|| format!("Meeting {meeting_id} was not found"))?;
    let event_id = format!("evt_{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();
    let payload = build_payload(pool, meeting, &event_id, &now).await?;

    let result = sqlx::query(
        "INSERT OR IGNORE INTO webhook_deliveries
         (event_id, event_type, meeting_id, payload, next_attempt_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(EVENT_TYPE)
    .bind(meeting_id)
    .bind(payload)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to enqueue webhook: {error}"))?;

    Ok(result.rows_affected() == 1)
}

async fn deliver_pending<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let config = load_config(app)?;
    if !config.enabled || validate_config(&config).is_err() {
        return Ok(());
    }
    let Some(state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    let pool = state.db_manager.pool();
    let deliveries = sqlx::query_as::<_, PendingDelivery>(
        "SELECT event_id, payload, attempt_count
         FROM webhook_deliveries
         WHERE delivered_at IS NULL AND next_attempt_at <= ?
         ORDER BY created_at ASC LIMIT 10",
    )
    .bind(Utc::now().to_rfc3339())
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Failed to load webhook outbox: {error}"))?;

    for delivery in deliveries {
        match post_payload(&config, &delivery.event_id, EVENT_TYPE, &delivery.payload).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE webhook_deliveries SET delivered_at = ?, last_error = NULL WHERE event_id = ?",
                )
                .bind(Utc::now().to_rfc3339())
                .bind(&delivery.event_id)
                .execute(pool)
                .await
                .map_err(|error| format!("Failed to mark webhook delivered: {error}"))?;
                log::info!("Delivered transcription webhook {}", delivery.event_id);
            }
            Err(error) => {
                let next_attempt = delivery.attempt_count + 1;
                let delay_seconds = (15_i64 * 2_i64.pow(next_attempt.min(8) as u32)).min(3600);
                sqlx::query(
                    "UPDATE webhook_deliveries
                     SET attempt_count = ?, next_attempt_at = ?, last_error = ?
                     WHERE event_id = ?",
                )
                .bind(next_attempt)
                .bind((Utc::now() + Duration::seconds(delay_seconds)).to_rfc3339())
                .bind(&error)
                .bind(&delivery.event_id)
                .execute(pool)
                .await
                .map_err(|db_error| format!("Failed to update webhook retry: {db_error}"))?;
                log::warn!("Webhook {} failed: {}", delivery.event_id, error);
            }
        }
    }
    Ok(())
}

pub fn init_worker<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            if let Err(error) = deliver_pending(&app).await {
                log::warn!("Webhook delivery pass failed: {}", error);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::models::DateTimeUtc;

    #[test]
    fn signs_payload_with_stable_sha256_hmac() {
        assert_eq!(
            sign_payload("0123456789abcdef", "{\"ok\":true}").unwrap(),
            "sha256=8a782523af5169f2186640bc66718bf0be9396ae14b3d22bc0b0d8a04af83e8d"
        );
    }

    #[test]
    fn requires_https_except_for_local_development() {
        let config = |endpoint: &str| WebhookConfig {
            enabled: true,
            endpoint: endpoint.to_string(),
            signing_secret: "0123456789abcdef".to_string(),
        };
        assert!(validate_config(&config("https://hooks.example.com/meetily")).is_ok());
        assert!(validate_config(&config("http://localhost:8787/webhook")).is_ok());
        assert!(validate_config(&config("http://hooks.example.com/meetily")).is_err());
    }

    async fn pool_with_transcripts(rows: &[(&str, &str, &str, Option<f64>, Option<f64>)]) -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE transcripts (
                id TEXT PRIMARY KEY,
                meeting_id TEXT NOT NULL,
                transcript TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                audio_start_time REAL,
                audio_end_time REAL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE transcript_settings (
                id TEXT PRIMARY KEY,
                provider TEXT,
                model TEXT,
                openaiApiKey TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (id, meeting_id, text, start, end) in rows {
            sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time)
                 VALUES (?, ?, ?, '2026-09-30T00:00:00Z', ?, ?)",
            )
            .bind(id)
            .bind(meeting_id)
            .bind(text)
            .bind(start)
            .bind(end)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn transcript_is_every_segment_in_spoken_order() {
        let pool = pool_with_transcripts(&[
            ("b", "m1", "  second line ", Some(4.0), Some(9.5)),
            ("a", "m1", "first line", Some(0.0), Some(4.0)),
            ("c", "m1", "   ", Some(9.5), Some(10.0)),
            ("d", "other", "not this meeting", Some(0.0), Some(99.0)),
        ])
        .await;

        let reading = read_transcript(&pool, "m1").await.unwrap();

        assert_eq!(
            reading,
            TranscriptReading {
                text: "first line\nsecond line".to_string(),
                segments: 3,
                duration_seconds: 10.0,
            }
        );
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value).unwrap().with_timezone(&Utc)
    }

    fn meeting(folder: Option<&Path>) -> MeetingModel {
        MeetingModel {
            id: "m1".to_string(),
            title: "Standup".to_string(),
            created_at: DateTimeUtc(at("2026-09-30T10:00:00Z")),
            updated_at: DateTimeUtc(at("2026-09-30T10:30:00Z")),
            folder_path: folder.map(|folder| folder.display().to_string()),
            pinned: false,
            archived: false,
            is_debug: false,
        }
    }

    fn write_metadata(folder: &Path, json: &str) {
        std::fs::write(folder.join("metadata.json"), json).unwrap();
    }

    async fn payload_for(pool: &SqlitePool, meeting: MeetingModel) -> serde_json::Value {
        let body = build_payload(pool, meeting, "evt_1", "2026-09-30T10:31:00+00:00")
            .await
            .unwrap();
        serde_json::from_str(&body).unwrap()
    }

    #[tokio::test]
    async fn payload_carries_the_transcript_end_and_model_the_meeting_recorded() {
        let pool = pool_with_transcripts(&[
            ("a", "m1", "hello", Some(0.0), Some(2.0)),
            ("b", "m1", "world", Some(2.0), Some(4.5)),
        ])
        .await;
        let folder = tempfile::tempdir().unwrap();
        write_metadata(
            folder.path(),
            r#"{"status":"completed","completed_at":"2026-09-30T10:25:00Z",
                "transcription_model":{"provider":"whisper","model":"large-v3"}}"#,
        );

        let payload = payload_for(&pool, meeting(Some(folder.path()))).await;

        assert_eq!(payload["meeting"]["started_at"], "2026-09-30T10:00:00+00:00");
        assert_eq!(payload["meeting"]["ended_at"], "2026-09-30T10:25:00+00:00");
        assert_eq!(payload["result"]["transcript"], "hello\nworld");
        assert_eq!(payload["result"]["transcript_segments"], 2);
        assert_eq!(payload["result"]["duration_seconds"], 4.5);
        assert_eq!(payload["result"]["transcription_model"], "whisper:large-v3");
    }

    // The model comes from the meeting, never the app's current settings: a
    // meeting retranscribed with another engine must not be stamped with this one.
    #[tokio::test]
    async fn payload_ignores_the_configured_engine() {
        let pool = pool_with_transcripts(&[("a", "m1", "hi", Some(0.0), Some(1.0))]).await;
        sqlx::query(
            "INSERT INTO transcript_settings (id, provider, model, openaiApiKey)
             VALUES ('1', 'deepgram', 'nova', 'sk-secret')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let folder = tempfile::tempdir().unwrap();
        write_metadata(folder.path(), r#"{"status":"completed"}"#);

        let payload = payload_for(&pool, meeting(Some(folder.path()))).await;
        let body = payload.to_string();

        assert!(payload["result"]["transcription_model"].is_null());
        assert!(!body.contains("deepgram") && !body.contains("sk-secret"));
    }

    #[tokio::test]
    async fn an_end_before_the_start_is_left_out() {
        let pool = pool_with_transcripts(&[]).await;
        let folder = tempfile::tempdir().unwrap();
        write_metadata(folder.path(), r#"{"completed_at":"2026-09-30T09:00:00Z"}"#);

        let payload = payload_for(&pool, meeting(Some(folder.path()))).await;

        assert!(payload["meeting"]["ended_at"].is_null());
    }

    #[tokio::test]
    async fn a_meeting_without_a_folder_or_segments_still_sends() {
        let pool = pool_with_transcripts(&[]).await;

        let payload = payload_for(&pool, meeting(None)).await;

        assert_eq!(payload["result"]["transcript"], "");
        assert_eq!(payload["result"]["transcript_segments"], 0);
        assert!(payload["meeting"]["ended_at"].is_null());
        assert!(payload["result"]["transcription_model"].is_null());
    }
}
