use crate::database::models::Transcript;
use crate::state::AppState;
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerIdentity {
    pub speaker_id: String,
    pub display_name: String,
    pub segment_count: usize,
    /// A few example lines from this speaker so the user can identify them.
    #[serde(default)]
    pub samples: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct IdentityRow {
    speaker_id: String,
    display_name: Option<String>,
    merged_into: Option<String>,
}

#[derive(sqlx::FromRow)]
struct OverrideRow {
    transcript_id: String,
    audio_start_time: Option<f64>,
    audio_end_time: Option<f64>,
    speaker_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSpeaker {
    pub speaker_id: String,
    pub display_name: String,
}

fn canonical_id(id: &str, identities: &HashMap<String, IdentityRow>) -> String {
    let mut current = id.to_string();
    let mut visited = HashSet::new();
    while visited.insert(current.clone()) {
        let Some(next) = identities
            .get(&current)
            .and_then(|identity| identity.merged_into.as_ref())
        else {
            break;
        };
        current.clone_from(next);
    }
    current
}

fn overlap(start_a: f64, end_a: f64, start_b: f64, end_b: f64) -> f64 {
    (end_a.min(end_b) - start_a.max(start_b)).max(0.0)
}

async fn correction_rows(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<(HashMap<String, IdentityRow>, Vec<OverrideRow>)> {
    let identities = sqlx::query_as::<_, IdentityRow>(
        "SELECT speaker_id, display_name, merged_into FROM speaker_identities WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| (row.speaker_id.clone(), row))
    .collect();
    let overrides = sqlx::query_as::<_, OverrideRow>(
        "SELECT transcript_id, audio_start_time, audio_end_time, speaker_id FROM speaker_segment_overrides WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;
    Ok((identities, overrides))
}

pub async fn resolve_transcript_speakers(
    pool: &SqlitePool,
    meeting_id: &str,
    transcripts: &[Transcript],
) -> Result<HashMap<String, ResolvedSpeaker>> {
    let (identities, overrides) = correction_rows(pool, meeting_id).await?;
    let mut resolved = HashMap::new();
    for transcript in transcripts {
        let direct = overrides
            .iter()
            .find(|item| item.transcript_id == transcript.id);
        let timed = match (transcript.audio_start_time, transcript.audio_end_time) {
            (Some(start), Some(end)) => overrides
                .iter()
                .filter_map(|item| {
                    let amount = overlap(start, end, item.audio_start_time?, item.audio_end_time?);
                    (amount > 0.0).then_some((item, amount))
                })
                .max_by(|left, right| left.1.total_cmp(&right.1))
                .map(|(item, _)| item),
            _ => None,
        };
        let Some(base_id) = direct
            .or(timed)
            .map(|item| item.speaker_id.as_str())
            .or(transcript.speaker.as_deref())
        else {
            continue;
        };
        let speaker_id = canonical_id(base_id, &identities);
        let display_name = identities
            .get(&speaker_id)
            .and_then(|identity| identity.display_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| speaker_id.clone());
        resolved.insert(
            transcript.id.clone(),
            ResolvedSpeaker {
                speaker_id,
                display_name,
            },
        );
    }
    Ok(resolved)
}

#[tauri::command]
pub async fn get_speaker_identities(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<SpeakerIdentity>, String> {
    let pool = state.db_manager.pool();
    let transcripts: Vec<Transcript> = sqlx::query_as(
        "SELECT * FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, timestamp",
    )
    .bind(&meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let resolved = resolve_transcript_speakers(pool, &meeting_id, &transcripts)
        .await
        .map_err(|error| error.to_string())?;
    const MAX_SAMPLES_PER_SPEAKER: usize = 3;
    const MAX_SAMPLE_CHARS: usize = 180;

    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    let mut samples: HashMap<String, Vec<String>> = HashMap::new();
    for transcript in &transcripts {
        let Some(speaker) = resolved.get(&transcript.id) else {
            continue;
        };
        let entry = counts
            .entry(speaker.speaker_id.clone())
            .or_insert_with(|| (speaker.display_name.clone(), 0));
        entry.1 += 1;

        let text = transcript.transcript.trim();
        if text.is_empty() {
            continue;
        }
        let bucket = samples.entry(speaker.speaker_id.clone()).or_default();
        if bucket.len() < MAX_SAMPLES_PER_SPEAKER {
            bucket.push(text.chars().take(MAX_SAMPLE_CHARS).collect());
        }
    }

    let mut result: Vec<_> = counts
        .into_iter()
        .map(
            |(speaker_id, (display_name, segment_count))| SpeakerIdentity {
                samples: samples.remove(&speaker_id).unwrap_or_default(),
                speaker_id,
                display_name,
                segment_count,
            },
        )
        .collect();
    result.sort_by(|left, right| left.speaker_id.cmp(&right.speaker_id));
    Ok(result)
}

fn validate_speaker_id(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 100 {
        return Err(anyhow!("Invalid speaker ID"));
    }
    Ok(())
}

#[tauri::command]
pub async fn rename_speaker(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    speaker_id: String,
    display_name: String,
) -> Result<(), String> {
    validate_speaker_id(&speaker_id).map_err(|error| error.to_string())?;
    let display_name = display_name.trim();
    if display_name.is_empty() || display_name.len() > 100 {
        return Err("Speaker name must be between 1 and 100 characters".into());
    }
    sqlx::query(
        "INSERT INTO speaker_identities (meeting_id, speaker_id, display_name, merged_into, updated_at) VALUES (?, ?, ?, NULL, ?) ON CONFLICT(meeting_id, speaker_id) DO UPDATE SET display_name = excluded.display_name, updated_at = excluded.updated_at",
    )
    .bind(&meeting_id)
    .bind(&speaker_id)
    .bind(display_name)
    .bind(Utc::now().to_rfc3339())
    .execute(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn merge_speakers(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    source_speaker_id: String,
    target_speaker_id: String,
) -> Result<(), String> {
    validate_speaker_id(&source_speaker_id).map_err(|error| error.to_string())?;
    validate_speaker_id(&target_speaker_id).map_err(|error| error.to_string())?;
    if source_speaker_id == target_speaker_id {
        return Err("Choose two different speakers to merge".into());
    }
    let pool = state.db_manager.pool();
    let (identities, _) = correction_rows(pool, &meeting_id)
        .await
        .map_err(|error| error.to_string())?;
    if canonical_id(&target_speaker_id, &identities)
        == canonical_id(&source_speaker_id, &identities)
    {
        return Err("These speakers are already merged".into());
    }
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE speaker_identities SET merged_into = ?, updated_at = ? WHERE meeting_id = ? AND merged_into = ?",
    )
    .bind(&target_speaker_id)
    .bind(&now)
    .bind(&meeting_id)
    .bind(&source_speaker_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    sqlx::query(
        "INSERT INTO speaker_identities (meeting_id, speaker_id, display_name, merged_into, updated_at) VALUES (?, ?, NULL, ?, ?) ON CONFLICT(meeting_id, speaker_id) DO UPDATE SET merged_into = excluded.merged_into, updated_at = excluded.updated_at",
    )
    .bind(&meeting_id)
    .bind(&source_speaker_id)
    .bind(&target_speaker_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn reassign_transcript_speaker(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    transcript_id: String,
    speaker_id: String,
) -> Result<(), String> {
    validate_speaker_id(&speaker_id).map_err(|error| error.to_string())?;
    let timing: Option<(Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT audio_start_time, audio_end_time FROM transcripts WHERE id = ? AND meeting_id = ?",
    )
    .bind(&transcript_id)
    .bind(&meeting_id)
    .fetch_optional(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())?;
    let (start, end) = timing.ok_or_else(|| "Transcript segment not found".to_string())?;
    sqlx::query(
        "INSERT INTO speaker_segment_overrides (id, meeting_id, transcript_id, audio_start_time, audio_end_time, speaker_id, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(meeting_id, transcript_id) DO UPDATE SET audio_start_time = excluded.audio_start_time, audio_end_time = excluded.audio_end_time, speaker_id = excluded.speaker_id, updated_at = excluded.updated_at",
    )
    .bind(format!("speaker-override-{}", Uuid::new_v4()))
    .bind(&meeting_id)
    .bind(&transcript_id)
    .bind(start)
    .bind(end)
    .bind(&speaker_id)
    .bind(Utc::now().to_rfc3339())
    .execute(state.db_manager.pool())
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(id: &str, start: f64, end: f64, speaker: &str) -> Transcript {
        Transcript {
            id: id.into(),
            meeting_id: "meeting".into(),
            transcript: id.into(),
            timestamp: "now".into(),
            speaker: Some(speaker.into()),
            summary: None,
            action_items: None,
            key_points: None,
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            duration: Some(end - start),
        }
    }

    #[test]
    fn canonical_mapping_follows_merges_and_stops_cycles() {
        let rows = HashMap::from([
            (
                "speaker_00".into(),
                IdentityRow {
                    speaker_id: "speaker_00".into(),
                    display_name: None,
                    merged_into: Some("speaker_01".into()),
                },
            ),
            (
                "speaker_01".into(),
                IdentityRow {
                    speaker_id: "speaker_01".into(),
                    display_name: Some("Avery".into()),
                    merged_into: None,
                },
            ),
        ]);
        assert_eq!(canonical_id("speaker_00", &rows), "speaker_01");
    }

    #[tokio::test]
    async fn resolves_renames_merges_and_timed_overrides_after_reprocessing() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE speaker_identities (meeting_id TEXT, speaker_id TEXT, display_name TEXT, merged_into TEXT, updated_at TEXT, PRIMARY KEY (meeting_id, speaker_id))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE speaker_segment_overrides (id TEXT PRIMARY KEY, meeting_id TEXT, transcript_id TEXT, audio_start_time REAL, audio_end_time REAL, speaker_id TEXT, updated_at TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO speaker_identities VALUES ('meeting', 'speaker_00', NULL, 'speaker_01', 'now'), ('meeting', 'speaker_01', 'Avery', NULL, 'now')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO speaker_segment_overrides VALUES ('override', 'meeting', 'old-transcript-id', 10.0, 12.0, 'speaker_00', 'now')")
            .execute(&pool)
            .await
            .unwrap();

        let regenerated = vec![transcript("new-transcript-id", 10.2, 11.8, "speaker_02")];
        let resolved = resolve_transcript_speakers(&pool, "meeting", &regenerated)
            .await
            .unwrap();
        assert_eq!(resolved["new-transcript-id"].speaker_id, "speaker_01");
        assert_eq!(resolved["new-transcript-id"].display_name, "Avery");
        assert_eq!(regenerated[0].transcript, "new-transcript-id");
        assert_eq!(regenerated[0].audio_start_time, Some(10.2));
    }
}
