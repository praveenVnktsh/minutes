//! Offline, local speaker diarization for completed recordings.
//!
//! The engine is FluidAudio's offline diarizer (pyannote community-1
//! segmentation + WeSpeaker embeddings + clustering) running on Core ML inside
//! the `speech-helper` sidecar, reached through [`crate::audio::speech::batch`].
//! The app downloads the model with the other speech models
//! ([`crate::audio::speech::models`]); this module never touches the network.

use crate::api::TranscriptSegment;
use crate::audio::common::write_transcripts_json;
use crate::audio::decoder::decode_audio_file;
use crate::audio::retranscription::find_audio_file;
use crate::audio::speech::{
    self,
    models::{Availability, ModelKind},
};
use crate::database::models::Transcript;
use crate::state::AppState;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

/// Names recorded with a diarization run so the saved result says which engine
/// and models actually produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EngineInfo {
    engine: &'static str,
    segmentation_model: &'static str,
    embedding_model: &'static str,
}

const FLUIDAUDIO_ENGINE: EngineInfo = EngineInfo {
    engine: "fluidaudio-0.18.0",
    segmentation_model: "pyannote-segmentation-community-1",
    embedding_model: "wespeaker-v2",
};

/// Shown when the diarization model has not been downloaded yet.
const MODEL_MISSING_MESSAGE: &str =
    "The speaker model is not downloaded. Download it in Settings → Transcription.";
/// Shown when the diarization model is still downloading; the run happens
/// once it is on disk.
const MODEL_DOWNLOADING_MESSAGE: &str =
    "The speaker model is still downloading; speakers are identified once it finishes.";

/// Payload for a meeting-scoped diarization progress event. Kept in one place so
/// every stage reports the same `meeting_id`, which is what lets the meeting
/// workspace attribute progress to the right meeting.
fn progress_payload(
    meeting_id: &str,
    stage: &str,
    progress_percentage: u32,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "meeting_id": meeting_id,
        "stage": stage,
        "progress_percentage": progress_percentage,
        "message": message,
    })
}

/// Emit a meeting-scoped diarization progress event.
fn emit_stage<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    stage: &str,
    progress_percentage: u32,
    message: &str,
) {
    let _ = app.emit(
        "diarization-progress",
        progress_payload(meeting_id, stage, progress_percentage, message),
    );
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakerTurn {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationResult {
    pub id: String,
    pub meeting_id: String,
    pub engine: String,
    pub segmentation_model: String,
    pub embedding_model: String,
    pub generated_at: String,
    pub speaker_count: usize,
    pub turns: Vec<SpeakerTurn>,
}

// ---------------------------------------------------------------------------
// FluidAudio diarizer
// ---------------------------------------------------------------------------

/// One speaker's stretch of audio as the diarizer reported it, in seconds.
/// `speaker` is numbered in order of first appearance.
#[derive(Debug, Clone, PartialEq)]
struct SpeakerSegment {
    start: f64,
    end: f64,
    speaker: u32,
}

/// Take the helper's segments, dropping any that are empty or not finite so a
/// malformed reply cannot produce a zero-length or NaN turn.
fn from_helper(segments: Vec<speech::batch::SpeakerSegment>) -> Vec<SpeakerSegment> {
    segments
        .into_iter()
        .filter(|segment| {
            segment.start.is_finite()
                && segment.end.is_finite()
                && segment.end > segment.start.max(0.0)
        })
        .map(|segment| SpeakerSegment {
            start: segment.start.max(0.0),
            end: segment.end,
            speaker: segment.speaker,
        })
        .collect()
}

/// Overall progress for a diarizer run that has processed `fraction` of the
/// audio, inside the 40..70 % "segmenting" band.
fn segmenting_stage_percentage(fraction: f32) -> u32 {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    40 + (fraction * 29.0).round() as u32
}

/// Keep at most `max_speakers` speakers, for callers that know how many people
/// were in the meeting. The helper is given the hint too, but FluidAudio may
/// not honour it, so it is enforced here: the `max_speakers` speakers with the
/// most total speech are kept, and each segment of any other speaker is handed
/// to the kept speaker whose speech is closest in time to it (gap between
/// segment edges; overlapping speech counts as distance zero; ties go to the
/// speaker with more speech). Surplus speakers are usually one person split in
/// two around their own turns, so the nearest kept speaker is the most
/// plausible owner. Same-speaker segments that end up overlapping or touching
/// are merged. The result is sorted by start, then speaker.
fn cap_speakers(segments: Vec<SpeakerSegment>, max_speakers: usize) -> Vec<SpeakerSegment> {
    let mut totals: HashMap<u32, f64> = HashMap::new();
    for segment in &segments {
        *totals.entry(segment.speaker).or_default() += segment.end - segment.start;
    }
    if totals.len() <= max_speakers || max_speakers == 0 {
        return segments;
    }
    let mut ranked: Vec<(u32, f64)> = totals.iter().map(|(&k, &v)| (k, v)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let kept: Vec<u32> = ranked
        .iter()
        .take(max_speakers)
        .map(|(speaker, _)| *speaker)
        .collect();

    let kept_segments: Vec<SpeakerSegment> = segments
        .iter()
        .filter(|segment| kept.contains(&segment.speaker))
        .cloned()
        .collect();
    let gap =
        |a: &SpeakerSegment, b: &SpeakerSegment| (a.start.max(b.start) - a.end.min(b.end)).max(0.0);
    let mut reassigned: Vec<SpeakerSegment> = segments
        .into_iter()
        .map(|mut segment| {
            if !kept.contains(&segment.speaker) {
                // `kept` is ordered by total speech, so `min_by` keeping the
                // first of equal distances breaks ties toward more speech.
                let nearest = kept
                    .iter()
                    .map(|&speaker| {
                        let distance = kept_segments
                            .iter()
                            .filter(|other| other.speaker == speaker)
                            .map(|other| gap(&segment, other))
                            .fold(f64::INFINITY, f64::min);
                        (speaker, distance)
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(speaker, _)| speaker);
                if let Some(speaker) = nearest {
                    segment.speaker = speaker;
                }
            }
            segment
        })
        .collect();

    reassigned.sort_by(|a, b| a.speaker.cmp(&b.speaker).then(a.start.total_cmp(&b.start)));
    let mut merged: Vec<SpeakerSegment> = Vec::with_capacity(reassigned.len());
    for segment in reassigned {
        match merged.last_mut() {
            Some(last) if last.speaker == segment.speaker && segment.start <= last.end => {
                last.end = last.end.max(segment.end);
            }
            _ => merged.push(segment),
        }
    }
    merged.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.speaker.cmp(&b.speaker)));
    merged
}

/// Convert diarizer segments to turns labelled `raw_NN`, the form label
/// stabilisation maps onto the meeting's `speaker_NN` IDs. FluidAudio reports
/// no per-segment confidence, so every turn carries 1.0.
fn segments_to_turns(segments: Vec<SpeakerSegment>) -> Vec<SpeakerTurn> {
    segments
        .into_iter()
        .map(|segment| SpeakerTurn {
            start: segment.start,
            end: segment.end,
            speaker: format!("raw_{:02}", segment.speaker),
            confidence: 1.0,
        })
        .collect()
}

/// Decode the meeting audio to 16 kHz mono off the async runtime.
async fn decode_meeting_audio<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    path: PathBuf,
) -> Result<Vec<f32>> {
    emit_stage(
        app,
        meeting_id,
        "decoding_audio",
        20,
        "Decoding meeting audio for speaker analysis...",
    );
    tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
        Ok(decode_audio_file(&path)?.to_whisper_format())
    })
    .await
    .map_err(|error| anyhow!("Audio decode task failed: {error}"))?
}

/// Diarize the meeting audio through the speech helper and report which engine
/// produced the turns.
async fn diarize_meeting_audio<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    audio_path: PathBuf,
    num_speakers: Option<usize>,
) -> Result<(Vec<SpeakerTurn>, EngineInfo)> {
    if !speech::models::diarization_ready(app) {
        bail!(MODEL_MISSING_MESSAGE);
    }
    let samples = decode_meeting_audio(app, meeting_id, audio_path).await?;
    emit_stage(
        app,
        meeting_id,
        "segmenting",
        segmenting_stage_percentage(0.0),
        "Detecting speakers...",
    );
    let progress_app = app.clone();
    let progress_meeting = meeting_id.to_string();
    let last = AtomicU32::new(u32::MAX);
    let on_progress: Box<dyn Fn(f32) + Send + Sync> = Box::new(move |fraction: f32| {
        let percentage = segmenting_stage_percentage(fraction);
        if last.swap(percentage, Ordering::Relaxed) != percentage {
            emit_stage(
                &progress_app,
                &progress_meeting,
                "segmenting",
                percentage,
                "Detecting speakers...",
            );
        }
    });
    let segments = speech::batch::diarize(&samples, num_speakers, Some(on_progress))
        .await
        .context("Speaker diarization failed")?;
    let segments = from_helper(segments);

    let segments = match num_speakers {
        Some(limit) => {
            let before = segments
                .iter()
                .map(|segment| segment.speaker)
                .collect::<HashSet<_>>()
                .len();
            if before > limit {
                log::info!(
                    "Diarization: found {} speakers; merging down to the requested {}",
                    before,
                    limit
                );
            }
            cap_speakers(segments, limit)
        }
        None => segments,
    };
    Ok((segments_to_turns(segments), FLUIDAUDIO_ENGINE))
}

fn overlap(start_a: f64, end_a: f64, start_b: f64, end_b: f64) -> f64 {
    (end_a.min(end_b) - start_a.max(start_b)).max(0.0)
}

/// A stretch of audio that an earlier diarization run gave a speaker ID.
#[derive(Debug, Clone, PartialEq)]
struct LabeledSpan {
    start: f64,
    end: f64,
    label: String,
}

/// A new cluster inherits an earlier ID only when they share at least this
/// fraction of the smaller one's speech. Below it the overlap is incidental
/// (a few seconds of crosstalk), and inheriting would hand that person's
/// rename to someone else.
const MIN_IDENTITY_OVERLAP: f64 = 0.3;

/// What the earlier assignment said, in recording time: the last run's turns
/// when there is one (they survive a final pass or retranscription, which
/// replaces the transcript rows and their speaker column), otherwise the
/// transcript rows that still carry `speaker_NN` labels.
fn previous_spans(
    previous_turns: Option<&[SpeakerTurn]>,
    transcripts: &[Transcript],
) -> Vec<LabeledSpan> {
    if let Some(turns) = previous_turns.filter(|turns| !turns.is_empty()) {
        return turns
            .iter()
            .filter(|turn| turn.speaker.starts_with("speaker_") && turn.end > turn.start)
            .map(|turn| LabeledSpan {
                start: turn.start,
                end: turn.end,
                label: turn.speaker.clone(),
            })
            .collect();
    }
    transcripts
        .iter()
        .filter_map(|transcript| {
            let label = transcript
                .speaker
                .as_deref()
                .filter(|value| value.starts_with("speaker_"))?;
            let (start, end) = (transcript.audio_start_time?, transcript.audio_end_time?);
            (end > start).then(|| LabeledSpan {
                start,
                end,
                label: label.to_string(),
            })
        })
        .collect()
}

/// Map a rerun's raw clusters onto the IDs of the previous assignment by
/// maximum time overlap (one-to-one, largest overlaps first), so renames,
/// merges and overrides keyed by those IDs stay with the same person. Clusters
/// with no convincing match get fresh IDs that avoid every `reserved` one
/// (earlier IDs and IDs the user corrected), so a new person never inherits
/// an old person's name.
fn stabilize_turn_labels(
    turns: &mut [SpeakerTurn],
    previous: &[LabeledSpan],
    reserved: &HashSet<String>,
) {
    let mut scores: HashMap<(String, String), f64> = HashMap::new();
    let mut new_totals: HashMap<String, f64> = HashMap::new();
    let mut old_totals: HashMap<String, f64> = HashMap::new();
    for span in previous {
        *old_totals.entry(span.label.clone()).or_default() += span.end - span.start;
    }
    for turn in turns.iter() {
        *new_totals.entry(turn.speaker.clone()).or_default() += (turn.end - turn.start).max(0.0);
        for span in previous {
            let amount = overlap(turn.start, turn.end, span.start, span.end);
            if amount > 0.0 {
                *scores
                    .entry((turn.speaker.clone(), span.label.clone()))
                    .or_default() += amount;
            }
        }
    }

    let mut candidates: Vec<_> = scores
        .into_iter()
        .filter(|((raw, previous), amount)| {
            let smaller = new_totals
                .get(raw)
                .copied()
                .unwrap_or(0.0)
                .min(old_totals.get(previous).copied().unwrap_or(0.0));
            smaller > 0.0 && *amount >= MIN_IDENTITY_OVERLAP * smaller
        })
        .collect();
    // Largest overlap first; ties broken by name so reruns are deterministic.
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut mapping = HashMap::new();
    let mut taken = HashSet::new();
    for ((raw, previous), _) in candidates {
        if !mapping.contains_key(&raw) && taken.insert(previous.clone()) {
            mapping.insert(raw, previous);
        }
    }

    let mut used_labels: HashSet<String> = reserved.clone();
    used_labels.extend(previous.iter().map(|span| span.label.clone()));
    let mut raw_labels: Vec<String> = turns.iter().map(|turn| turn.speaker.clone()).collect();
    raw_labels.sort();
    raw_labels.dedup();
    let mut next = 0usize;
    for raw in raw_labels {
        if mapping.contains_key(&raw) {
            continue;
        }
        loop {
            let label = format!("speaker_{next:02}");
            next += 1;
            if used_labels.insert(label.clone()) {
                mapping.insert(raw.clone(), label);
                break;
            }
        }
    }
    for turn in turns {
        if let Some(label) = mapping.get(&turn.speaker) {
            turn.speaker.clone_from(label);
        }
    }
}

/// The latest saved run's turns for a meeting, if one parses.
async fn latest_run_turns(pool: &sqlx::SqlitePool, meeting_id: &str) -> Option<Vec<SpeakerTurn>> {
    let json: Option<String> = sqlx::query_scalar(
        "SELECT result_json FROM diarization_runs WHERE meeting_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| log::warn!("Diarization: could not read the previous run: {error}"))
    .ok()
    .flatten();
    serde_json::from_str::<DiarizationResult>(&json?)
        .map_err(|error| log::warn!("Diarization: the previous run is unreadable: {error}"))
        .ok()
        .map(|result| result.turns)
}

/// IDs the user has named, merged or pinned segments to: a new cluster must
/// never be handed one of these.
async fn corrected_speaker_ids(pool: &sqlx::SqlitePool, meeting_id: &str) -> HashSet<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT speaker_id FROM speaker_identities WHERE meeting_id = ? \
         UNION SELECT merged_into FROM speaker_identities WHERE meeting_id = ? AND merged_into IS NOT NULL \
         UNION SELECT speaker_id FROM speaker_segment_overrides WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .bind(meeting_id)
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .unwrap_or_else(|error| {
        log::warn!("Diarization: could not read speaker corrections: {error}");
        Vec::new()
    });
    rows.into_iter().collect()
}

fn speaker_for_transcript(transcript: &Transcript, turns: &[SpeakerTurn]) -> Option<String> {
    let (start, end) = (transcript.audio_start_time?, transcript.audio_end_time?);
    let overlapping = turns
        .iter()
        .map(|turn| (turn, overlap(start, end, turn.start, turn.end)))
        .filter(|(_, amount)| *amount > 0.0)
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(turn, _)| turn.speaker.clone());
    overlapping.or_else(|| {
        let midpoint = (start + end) / 2.0;
        turns
            .iter()
            .min_by(|left, right| {
                let left_distance = if midpoint < left.start {
                    left.start - midpoint
                } else if midpoint > left.end {
                    midpoint - left.end
                } else {
                    0.0
                };
                let right_distance = if midpoint < right.start {
                    right.start - midpoint
                } else if midpoint > right.end {
                    midpoint - right.end
                } else {
                    0.0
                };
                left_distance.total_cmp(&right_distance)
            })
            .map(|turn| turn.speaker.clone())
    })
}

/// Resolve the final speaker label for a transcript, promoting the user's own
/// cluster to `mic` (which the UI renders as "You") when we are confident.
fn resolved_speaker(
    transcript: &Transcript,
    turns: &[SpeakerTurn],
    user_label: Option<&str>,
) -> Option<String> {
    let assigned = speaker_for_transcript(transcript, turns).or_else(|| transcript.speaker.clone());
    match (assigned.as_deref(), user_label) {
        (Some(label), Some(user)) if label == user => Some("mic".to_string()),
        _ => assigned,
    }
}

/// Identify the user's cluster from the microphone channel.
///
/// Before diarization the `speaker` column still holds `mic`/`system` (the
/// capture channel). If every microphone segment maps to exactly one cluster and
/// that cluster does not also appear on the system channel, it is unambiguously
/// the user. Multiple people in one room (several mic clusters) stay unlabeled.
fn detect_user_speaker_label(transcripts: &[Transcript], turns: &[SpeakerTurn]) -> Option<String> {
    let mut mic_labels = HashSet::new();
    let mut system_labels = HashSet::new();
    for transcript in transcripts {
        let Some(label) = speaker_for_transcript(transcript, turns) else {
            continue;
        };
        match transcript.speaker.as_deref() {
            Some("mic") => {
                mic_labels.insert(label);
            }
            Some("system") => {
                system_labels.insert(label);
            }
            _ => {}
        }
    }

    if mic_labels.len() != 1 {
        return None;
    }
    let label = mic_labels.into_iter().next()?;
    (!system_labels.contains(&label)).then_some(label)
}

async fn persist<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    folder: &Path,
    transcripts: &[Transcript],
    result: &DiarizationResult,
    user_label: Option<&str>,
) -> Result<()> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("App state not available"))?;
    let pool = state.db_manager.pool();
    let mut tx = pool.begin().await?;
    for transcript in transcripts {
        let speaker = resolved_speaker(transcript, &result.turns, user_label);
        sqlx::query("UPDATE transcripts SET speaker = ? WHERE id = ? AND meeting_id = ?")
            .bind(speaker)
            .bind(&transcript.id)
            .bind(meeting_id)
            .execute(&mut *tx)
            .await?;
    }

    // Seed a friendly name for the user's channel without clobbering a rename.
    if user_label.is_some() {
        sqlx::query(
            "INSERT INTO speaker_identities (meeting_id, speaker_id, display_name, merged_into, updated_at) \
             VALUES (?, 'mic', 'You', NULL, ?) \
             ON CONFLICT(meeting_id, speaker_id) DO NOTHING",
        )
        .bind(meeting_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("INSERT INTO diarization_runs (id, meeting_id, engine, segmentation_model, embedding_model, result_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&result.id)
        .bind(meeting_id)
        .bind(&result.engine)
        .bind(&result.segmentation_model)
        .bind(&result.embedding_model)
        .bind(serde_json::to_string(result)?)
        .bind(&result.generated_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let updated: Vec<TranscriptSegment> = transcripts
        .iter()
        .map(|transcript| TranscriptSegment {
            id: transcript.id.clone(),
            text: transcript.transcript.clone(),
            timestamp: transcript.timestamp.clone(),
            speaker: resolved_speaker(transcript, &result.turns, user_label),
            audio_start_time: transcript.audio_start_time,
            audio_end_time: transcript.audio_end_time,
            duration: transcript.duration,
        })
        .collect();
    write_transcripts_json(folder, &updated)?;
    let temp_path = folder.join(".diarization.json.tmp");
    tokio::fs::write(&temp_path, serde_json::to_vec_pretty(result)?).await?;
    tokio::fs::rename(temp_path, folder.join("diarization.json")).await?;
    Ok(())
}

/// Meetings being diarized in this process. `diarization_state` alone cannot
/// tell a running meeting from one whose run died with the app, so the status
/// command checks this first.
static RUNNING: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

/// Holds a meeting's slot in [`RUNNING`] and frees it on drop, so an early
/// return or a panic cannot leave the meeting stuck as running.
struct RunningSlot(String);

impl RunningSlot {
    fn acquire(meeting_id: &str) -> Result<Self> {
        let mut running = RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !running
            .get_or_insert_with(HashSet::new)
            .insert(meeting_id.to_string())
        {
            bail!("Speaker identification is already running for this meeting");
        }
        Ok(Self(meeting_id.to_string()))
    }
}

impl Drop for RunningSlot {
    fn drop(&mut self) {
        let mut running = RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = running.as_mut() {
            running.remove(&self.0);
        }
    }
}

fn is_running(meeting_id: &str) -> bool {
    RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|running| running.contains(meeting_id))
}

/// Tell the frontend a meeting's [`DiarizationStatus`] may have changed.
fn emit_status_changed<R: Runtime>(app: &AppHandle<R>, meeting_id: &str) {
    let _ = app.emit(
        "diarization-status-changed",
        serde_json::json!({ "meeting_id": meeting_id }),
    );
}

/// Record that a meeting's transcript rows were replaced and need speakers
/// again. Runs inside the caller's transaction so the two land together.
pub(crate) async fn mark_pending_in_tx(
    tx: &mut sqlx::SqliteConnection,
    meeting_id: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO diarization_state (meeting_id, status, error, attempts, updated_at) \
         VALUES (?, 'pending', NULL, 0, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET status = 'pending', error = NULL, attempts = 0, \
         updated_at = excluded.updated_at",
    )
    .bind(meeting_id)
    .bind(Utc::now().to_rfc3339())
    .execute(tx)
    .await?;
    Ok(())
}

/// Owe a run without counting an attempt: the model is still downloading, and
/// the resume runs it once it is on disk.
async fn record_waiting(pool: &sqlx::SqlitePool, meeting_id: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO diarization_state (meeting_id, status, error, attempts, updated_at) \
         VALUES (?, 'pending', NULL, 0, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET status = 'pending', error = NULL, \
         updated_at = excluded.updated_at",
    )
    .bind(meeting_id)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a run as started: still pending, one more attempt.
async fn record_started(pool: &sqlx::SqlitePool, meeting_id: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO diarization_state (meeting_id, status, error, attempts, updated_at) \
         VALUES (?, 'pending', NULL, 1, ?) \
         ON CONFLICT(meeting_id) DO UPDATE SET status = 'pending', error = NULL, \
         attempts = diarization_state.attempts + 1, updated_at = excluded.updated_at",
    )
    .bind(meeting_id)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Record how a run ended. Success resets the attempt count.
async fn record_finished(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    error: Option<&str>,
) -> Result<()> {
    let (status, reset_attempts) = match error {
        None => ("done", true),
        Some(_) => ("failed", false),
    };
    sqlx::query(
        "UPDATE diarization_state SET status = ?, error = ?, \
         attempts = CASE WHEN ? THEN 0 ELSE attempts END, updated_at = ? \
         WHERE meeting_id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(reset_attempts)
    .bind(Utc::now().to_rfc3339())
    .bind(meeting_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Diarize a meeting and persist the speakers, tracking the run in
/// `diarization_state` so a failure or a quit mid-run can be retried.
pub async fn run_for_meeting<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    num_speakers: Option<usize>,
) -> Result<DiarizationResult> {
    if matches!(num_speakers, Some(0)) {
        return Err(anyhow!("num_speakers must be greater than zero"));
    }
    let pool = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("App state not available"))?
        .db_manager
        .pool()
        .clone();
    if speech::models::availability(app, &[ModelKind::Diarization]) == Availability::Coming {
        // Pending, not failed: the resume runs it when the download finishes.
        if let Err(error) = record_waiting(&pool, meeting_id).await {
            log::warn!(
                "Diarization: could not record the wait for meeting {meeting_id}: {error:#}"
            );
        }
        emit_status_changed(app, meeting_id);
        log::info!("Diarization for meeting {meeting_id} waits for the speaker model");
        bail!(MODEL_DOWNLOADING_MESSAGE);
    }
    let slot = RunningSlot::acquire(meeting_id)?;
    if let Err(error) = record_started(&pool, meeting_id).await {
        log::warn!("Diarization: could not record start for meeting {meeting_id}: {error:#}");
    }
    emit_status_changed(app, meeting_id);

    let outcome = diarize_and_persist(app, meeting_id, num_speakers).await;
    let error = outcome.as_ref().err().map(|error| format!("{error:#}"));
    if let Err(record_error) = record_finished(&pool, meeting_id, error.as_deref()).await {
        log::warn!(
            "Diarization: could not record result for meeting {meeting_id}: {record_error:#}"
        );
    }
    if let Some(error) = &error {
        log::warn!("Diarization failed for meeting {meeting_id}: {error}");
    }
    drop(slot);
    emit_status_changed(app, meeting_id);
    outcome
}

async fn diarize_and_persist<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    num_speakers: Option<usize>,
) -> Result<DiarizationResult> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("App state not available"))?;
    let pool = state.db_manager.pool();
    let folder_path: String =
        sqlx::query_scalar::<_, Option<String>>("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await?
            .flatten()
            .ok_or_else(|| anyhow!("Meeting has no recording folder"))?;
    let transcripts: Vec<Transcript> = sqlx::query_as(
        "SELECT * FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time, timestamp",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;
    if transcripts.is_empty() {
        return Err(anyhow!("Meeting has no transcript segments"));
    }
    let folder = PathBuf::from(folder_path);
    let audio_path = find_audio_file(&folder)?;
    let (mut turns, engine) =
        diarize_meeting_audio(app, meeting_id, audio_path, num_speakers).await?;
    if turns.is_empty() {
        return Err(anyhow!("No speaker turns were detected"));
    }
    log::info!(
        "Diarization: {} produced {} turn(s) for meeting {}",
        engine.engine,
        turns.len(),
        meeting_id
    );
    emit_stage(
        app,
        meeting_id,
        "clustering",
        70,
        "Grouping speaker turns...",
    );
    let previous_turns = latest_run_turns(pool, meeting_id).await;
    let previous = previous_spans(previous_turns.as_deref(), &transcripts);
    let reserved = corrected_speaker_ids(pool, meeting_id).await;
    stabilize_turn_labels(&mut turns, &previous, &reserved);
    let user_label = detect_user_speaker_label(&transcripts, &turns);
    if let Some(label) = &user_label {
        log::info!(
            "Diarization: mic channel maps to a single cluster ({}); labeling it as the user",
            label
        );
    }
    let speaker_count = turns
        .iter()
        .map(|turn| &turn.speaker)
        .collect::<HashSet<_>>()
        .len();
    let result = DiarizationResult {
        id: format!("diarization-{}", Uuid::new_v4()),
        meeting_id: meeting_id.to_string(),
        engine: engine.engine.to_string(),
        segmentation_model: engine.segmentation_model.to_string(),
        embedding_model: engine.embedding_model.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        speaker_count,
        turns,
    };
    emit_stage(
        app,
        meeting_id,
        "saving_speakers",
        85,
        "Saving speaker labels...",
    );
    persist(
        app,
        meeting_id,
        &folder,
        &transcripts,
        &result,
        user_label.as_deref(),
    )
    .await
    .context("Failed to persist diarization output")?;
    crate::audio::common::emit_transcripts_updated(app, meeting_id);
    emit_stage(
        app,
        meeting_id,
        "speakers_complete",
        100,
        "Speaker identification complete",
    );
    let _ = app.emit("diarization-complete", &result);
    Ok(result)
}

/// Whether a meeting's current transcript has speaker labels, for the
/// transcript panel's status and retry affordance.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DiarizationStatus {
    /// Speakers are applied to the current transcript.
    Done,
    /// A run is in progress in this process.
    Running,
    /// A run is owed but not running: it was interrupted (the app quit
    /// mid-run) or has not started yet.
    Pending { attempts: i64 },
    /// The last run failed.
    Failed { error: String },
    /// No run was ever recorded, e.g. an imported meeting.
    Missing,
}

async fn diarization_status(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
) -> Result<DiarizationStatus> {
    if is_running(meeting_id) {
        return Ok(DiarizationStatus::Running);
    }
    let row: Option<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT status, error, attempts FROM diarization_state WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((status, error, attempts)) => match status.as_str() {
            "done" => DiarizationStatus::Done,
            "failed" => DiarizationStatus::Failed {
                error: error.unwrap_or_else(|| "Unknown error".to_string()),
            },
            _ => DiarizationStatus::Pending { attempts },
        },
        // Meetings diarized before this table existed only have a run.
        None => {
            let has_run: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM diarization_runs WHERE meeting_id = ?)",
            )
            .bind(meeting_id)
            .fetch_one(pool)
            .await?;
            if has_run {
                DiarizationStatus::Done
            } else {
                DiarizationStatus::Missing
            }
        }
    })
}

#[tauri::command]
pub async fn get_diarization_status<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
) -> Result<DiarizationStatus, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "App state not available".to_string())?;
    diarization_status(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|error| error.to_string())
}

/// Wait before resuming so startup (model loads, the first window) is not
/// competing with a CPU-heavy diarization run.
const RESUME_DELAY: Duration = Duration::from_secs(30);
/// Stop resuming a meeting after this many runs without a success: it keeps
/// failing or crashing the app, and the panel offers a manual retry instead.
const RESUME_MAX_ATTEMPTS: i64 = 3;
/// Only resume meetings whose diarization was owed within this window.
const RESUME_WINDOW_DAYS: i64 = 7;

/// Meetings whose diarization was owed when the app last quit, oldest first.
async fn interrupted_meetings(pool: &sqlx::SqlitePool) -> Result<Vec<String>> {
    let since = (Utc::now() - chrono::Duration::days(RESUME_WINDOW_DAYS)).to_rfc3339();
    Ok(sqlx::query_scalar(
        "SELECT meeting_id FROM diarization_state \
         WHERE status = 'pending' AND attempts < ? AND updated_at >= ? \
         ORDER BY updated_at",
    )
    .bind(RESUME_MAX_ATTEMPTS)
    .bind(since)
    .fetch_all(pool)
    .await?)
}

/// Resume diarization the app was doing when it last quit. Runs one meeting
/// at a time in the background and waits out any live recording first.
pub fn init_resume_worker<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(RESUME_DELAY).await;
        resume_pending(&app).await;
    });
}

/// Run the owed diarizations now: the speaker model just finished
/// downloading.
pub fn spawn_resume<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        resume_pending(&app).await;
    });
}

/// Serialises [`resume_pending`], so startup and a finished download never
/// walk the same meetings at once.
static RESUME_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn resume_pending<R: Runtime>(app: &AppHandle<R>) {
    let _turn = RESUME_TURN.lock().await;
    // While the speaker model downloads, nothing runs and no attempt is
    // spent; the download's end calls this again.
    if speech::models::availability(app, &[ModelKind::Diarization]) == Availability::Coming {
        log::info!("Diarization resume: waiting for the speaker model to download");
        return;
    }
    // The database is not set up until onboarding finishes.
    let Some(pool) = app
        .try_state::<AppState>()
        .map(|state| state.db_manager.pool().clone())
    else {
        return;
    };
    let meetings = match interrupted_meetings(&pool).await {
        Ok(meetings) => meetings,
        Err(error) => {
            log::warn!("Diarization resume: could not list interrupted meetings: {error:#}");
            return;
        }
    };
    if meetings.is_empty() {
        return;
    }
    log::info!(
        "Diarization resume: {} meeting(s) were interrupted, resuming",
        meetings.len()
    );
    for meeting_id in meetings {
        while crate::audio::recording_commands::has_recording_session() {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        if is_running(&meeting_id) {
            continue;
        }
        match run_for_meeting(app, &meeting_id, None).await {
            Ok(result) => log::info!(
                "Diarization resume: meeting {meeting_id} has {} speaker(s)",
                result.speaker_count
            ),
            // run_for_meeting already logged and recorded the failure.
            Err(_) => {}
        }
    }
    // Outside a transcription job nothing else frees the batch helper.
    crate::audio::common::release_batch_helper(None).await;
}

#[tauri::command]
pub async fn run_speaker_diarization<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    num_speakers: Option<usize>,
) -> Result<DiarizationResult, String> {
    let outcome = run_for_meeting(&app, &meeting_id, num_speakers).await;
    // A manual run is not part of a transcription job, whose end would
    // otherwise drop the batch helper and the ~2 GB of models it holds.
    crate::audio::common::release_batch_helper(None).await;
    outcome.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state_pool() -> sqlx::SqlitePool {
        use sqlx::Executor;
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        pool.execute(
            "CREATE TABLE meetings (id TEXT PRIMARY KEY NOT NULL); \
             INSERT INTO meetings VALUES ('m1'), ('m2'), ('m3'); \
             CREATE TABLE diarization_runs (id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL);",
        )
        .await
        .unwrap();
        pool.execute(include_str!(
            "../../migrations/20260924000000_add_diarization_state.sql"
        ))
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn status_follows_a_meeting_through_failure_retry_and_retranscription() {
        let pool = state_pool().await;
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Missing
        );

        record_started(&pool, "m1").await.unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Pending { attempts: 1 },
            "a started run with no live slot reads as interrupted"
        );
        {
            let _slot = RunningSlot::acquire("m1").unwrap();
            assert_eq!(
                diarization_status(&pool, "m1").await.unwrap(),
                DiarizationStatus::Running
            );
        }

        record_finished(&pool, "m1", Some("No speaker turns were detected"))
            .await
            .unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Failed {
                error: "No speaker turns were detected".into()
            }
        );

        record_started(&pool, "m1").await.unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Pending { attempts: 2 }
        );
        record_finished(&pool, "m1", None).await.unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Done
        );

        // A retranscription replaces the rows, so speakers are owed again.
        let mut conn = pool.acquire().await.unwrap();
        mark_pending_in_tx(&mut conn, "m1").await.unwrap();
        drop(conn);
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Pending { attempts: 0 }
        );
    }

    #[tokio::test]
    async fn meetings_diarized_before_state_tracking_read_as_done() {
        let pool = state_pool().await;
        sqlx::query("INSERT INTO diarization_runs VALUES ('run', 'm2')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            diarization_status(&pool, "m2").await.unwrap(),
            DiarizationStatus::Done
        );
    }

    #[tokio::test]
    async fn resume_skips_finished_repeatedly_failing_and_stale_meetings() {
        let pool = state_pool().await;
        // m1: interrupted once — resumed.
        record_started(&pool, "m1").await.unwrap();
        // m2: started and cut off RESUME_MAX_ATTEMPTS times — given up on.
        for _ in 0..RESUME_MAX_ATTEMPTS {
            record_started(&pool, "m2").await.unwrap();
        }
        // m3: finished — nothing to resume.
        record_started(&pool, "m3").await.unwrap();
        record_finished(&pool, "m3", None).await.unwrap();
        assert_eq!(
            interrupted_meetings(&pool).await.unwrap(),
            vec!["m1".to_string()]
        );

        // A pending meeting older than the window is left for a manual retry.
        let stale = (Utc::now() - chrono::Duration::days(RESUME_WINDOW_DAYS + 1)).to_rfc3339();
        sqlx::query("UPDATE diarization_state SET updated_at = ? WHERE meeting_id = 'm1'")
            .bind(stale)
            .execute(&pool)
            .await
            .unwrap();
        assert!(interrupted_meetings(&pool).await.unwrap().is_empty());
    }

    #[test]
    fn a_meeting_runs_once_at_a_time() {
        let slot = RunningSlot::acquire("slot-test").unwrap();
        assert!(is_running("slot-test"));
        assert!(RunningSlot::acquire("slot-test").is_err());
        drop(slot);
        assert!(!is_running("slot-test"));
        drop(RunningSlot::acquire("slot-test").unwrap());
    }

    fn transcript(id: &str, start: f64, end: f64, speaker: Option<&str>) -> Transcript {
        Transcript {
            id: id.into(),
            meeting_id: "meeting".into(),
            transcript: id.into(),
            timestamp: "now".into(),
            speaker: speaker.map(str::to_string),
            summary: None,
            action_items: None,
            key_points: None,
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            duration: Some(end - start),
        }
    }

    #[test]
    fn assigns_by_largest_timestamp_overlap() {
        let turns = vec![
            SpeakerTurn {
                start: 0.0,
                end: 2.0,
                speaker: "speaker_00".into(),
                confidence: 1.0,
            },
            SpeakerTurn {
                start: 2.0,
                end: 6.0,
                speaker: "speaker_01".into(),
                confidence: 1.0,
            },
        ];
        assert_eq!(
            speaker_for_transcript(&transcript("a", 1.5, 4.0, None), &turns),
            Some("speaker_01".into())
        );
    }

    #[test]
    fn rerun_preserves_previous_ids_by_overlap() {
        let transcripts = vec![
            transcript("a", 0.0, 2.0, Some("speaker_03")),
            transcript("b", 2.0, 4.0, Some("speaker_01")),
        ];
        let mut turns = vec![
            SpeakerTurn {
                start: 0.0,
                end: 2.0,
                speaker: "raw_01".into(),
                confidence: 1.0,
            },
            SpeakerTurn {
                start: 2.0,
                end: 4.0,
                speaker: "raw_00".into(),
                confidence: 1.0,
            },
        ];
        stabilize_turn_labels(
            &mut turns,
            &previous_spans(None, &transcripts),
            &HashSet::new(),
        );
        assert_eq!(turns[0].speaker, "speaker_03");
        assert_eq!(turns[1].speaker, "speaker_01");
    }

    fn turn(start: f64, end: f64, speaker: &str) -> SpeakerTurn {
        SpeakerTurn {
            start,
            end,
            speaker: speaker.into(),
            confidence: 1.0,
        }
    }

    #[test]
    fn rerun_after_a_final_pass_keeps_ids_from_the_previous_run() {
        // Regression: the final pass replaces the transcript rows, so their
        // speaker column is empty and the rerun used to number clusters from
        // scratch. Raw cluster order flipped here, which moved "Avery"
        // (speaker_01) onto the other person.
        let previous_run = vec![
            turn(0.0, 10.0, "speaker_00"),
            turn(10.0, 20.0, "speaker_01"),
            turn(20.0, 30.0, "speaker_00"),
        ];
        let replaced_rows = vec![
            transcript("new-a", 0.5, 9.5, None),
            transcript("new-b", 10.5, 19.5, None),
        ];
        let mut turns = vec![
            turn(0.0, 10.2, "raw_01"),
            turn(10.2, 20.0, "raw_00"),
            turn(20.0, 30.0, "raw_01"),
        ];
        let previous = previous_spans(Some(&previous_run), &replaced_rows);
        stabilize_turn_labels(&mut turns, &previous, &HashSet::new());
        assert_eq!(turns[0].speaker, "speaker_00");
        assert_eq!(turns[1].speaker, "speaker_01");
        assert_eq!(turns[2].speaker, "speaker_00");
    }

    #[test]
    fn a_new_speaker_never_inherits_a_corrected_or_earlier_id() {
        // A resumed meeting: speaker_01 ("Avery") spoke 10-20 s. The rerun
        // finds a new person from 20 s on whose first turn grazes Avery's
        // last second; a second of overlap must not hand them Avery's name.
        let previous_run = vec![
            turn(0.0, 10.0, "speaker_00"),
            turn(10.0, 20.0, "speaker_01"),
        ];
        let mut turns = vec![
            turn(0.0, 10.0, "raw_00"),
            turn(11.0, 20.0, "raw_02"),
            turn(19.0, 60.0, "raw_01"),
        ];
        let reserved = HashSet::from(["speaker_02".to_string()]);
        stabilize_turn_labels(
            &mut turns,
            &previous_spans(Some(&previous_run), &[]),
            &reserved,
        );
        assert_eq!(turns[0].speaker, "speaker_00");
        assert_eq!(turns[1].speaker, "speaker_01");
        // speaker_02 carries a user correction (e.g. a merge target), so the
        // new person gets the next free ID.
        assert_eq!(turns[2].speaker, "speaker_03");
    }

    #[test]
    fn detects_user_when_mic_is_a_single_cluster() {
        let transcripts = vec![
            transcript("a", 0.0, 2.0, Some("mic")),
            transcript("b", 3.0, 5.0, Some("mic")),
            transcript("c", 6.0, 8.0, Some("system")),
        ];
        let turns = vec![
            SpeakerTurn {
                start: 0.0,
                end: 2.5,
                speaker: "speaker_00".into(),
                confidence: 1.0,
            },
            SpeakerTurn {
                start: 3.0,
                end: 5.0,
                speaker: "speaker_00".into(),
                confidence: 1.0,
            },
            SpeakerTurn {
                start: 6.0,
                end: 8.0,
                speaker: "speaker_01".into(),
                confidence: 1.0,
            },
        ];
        assert_eq!(
            detect_user_speaker_label(&transcripts, &turns),
            Some("speaker_00".to_string())
        );
    }

    #[test]
    fn skips_user_when_mic_has_multiple_speakers() {
        let transcripts = vec![
            transcript("a", 0.0, 2.0, Some("mic")),
            transcript("b", 3.0, 5.0, Some("mic")),
        ];
        let turns = vec![
            SpeakerTurn {
                start: 0.0,
                end: 2.5,
                speaker: "speaker_00".into(),
                confidence: 1.0,
            },
            SpeakerTurn {
                start: 3.0,
                end: 5.0,
                speaker: "speaker_01".into(),
                confidence: 1.0,
            },
        ];
        assert_eq!(detect_user_speaker_label(&transcripts, &turns), None);
    }

    #[test]
    fn skips_user_when_mic_cluster_also_appears_on_system() {
        let transcripts = vec![
            transcript("a", 0.0, 2.0, Some("mic")),
            transcript("c", 1.0, 2.0, Some("system")),
        ];
        let turns = vec![SpeakerTurn {
            start: 0.0,
            end: 2.5,
            speaker: "speaker_00".into(),
            confidence: 1.0,
        }];
        assert_eq!(detect_user_speaker_label(&transcripts, &turns), None);
    }

    #[test]
    fn resolved_speaker_promotes_user_cluster_to_mic() {
        let turns = vec![
            SpeakerTurn {
                start: 0.0,
                end: 2.0,
                speaker: "speaker_00".into(),
                confidence: 1.0,
            },
            SpeakerTurn {
                start: 2.0,
                end: 4.0,
                speaker: "speaker_01".into(),
                confidence: 1.0,
            },
        ];
        assert_eq!(
            resolved_speaker(
                &transcript("a", 0.0, 1.5, Some("mic")),
                &turns,
                Some("speaker_00")
            ),
            Some("mic".to_string())
        );
        assert_eq!(
            resolved_speaker(
                &transcript("b", 2.0, 3.5, Some("system")),
                &turns,
                Some("speaker_00")
            ),
            Some("speaker_01".to_string())
        );
    }

    #[test]
    fn progress_payload_is_meeting_scoped() {
        let payload = progress_payload("meeting-1", "segmenting", 40, "Detecting speakers...");
        assert_eq!(payload["meeting_id"], "meeting-1");
        assert_eq!(payload["stage"], "segmenting");
        assert_eq!(payload["progress_percentage"], 40);
        assert_eq!(payload["message"], "Detecting speakers...");
    }

    fn segment(start: f64, end: f64, speaker: u32) -> SpeakerSegment {
        SpeakerSegment {
            start,
            end,
            speaker,
        }
    }

    #[test]
    fn helper_segments_drop_empty_and_non_finite_spans() {
        let helper = |start: f64, end: f64, speaker: u32| speech::batch::SpeakerSegment {
            speaker,
            start,
            end,
        };
        let segments = from_helper(vec![
            helper(0.5, 1.25, 0),
            helper(2.0, 2.0, 1),
            helper(3.0, 2.5, 1),
            helper(f64::NAN, 4.0, 1),
            helper(-0.1, 0.4, 2),
            helper(-1.0, -0.5, 3),
        ]);
        assert_eq!(segments, vec![segment(0.5, 1.25, 0), segment(0.0, 0.4, 2)]);
    }

    #[test]
    fn segments_map_to_raw_labelled_turns() {
        let turns = segments_to_turns(vec![segment(0.5, 1.25, 0), segment(1.0, 3.0, 7)]);
        assert_eq!(
            turns,
            vec![
                SpeakerTurn {
                    start: 0.5,
                    end: 1.25,
                    speaker: "raw_00".into(),
                    confidence: 1.0,
                },
                SpeakerTurn {
                    start: 1.0,
                    end: 3.0,
                    speaker: "raw_07".into(),
                    confidence: 1.0,
                },
            ]
        );
    }

    #[test]
    fn diarizer_turns_feed_label_stabilisation() {
        let mut turns = segments_to_turns(vec![segment(0.0, 2.0, 1), segment(2.0, 4.0, 0)]);
        stabilize_turn_labels(&mut turns, &[], &HashSet::new());
        assert_eq!(turns[0].speaker, "speaker_01");
        assert_eq!(turns[1].speaker, "speaker_00");
    }

    #[test]
    fn cap_speakers_is_a_no_op_within_the_limit() {
        let segments = vec![segment(0.0, 1.0, 0), segment(1.0, 2.0, 1)];
        assert_eq!(cap_speakers(segments.clone(), 2), segments);
        assert_eq!(cap_speakers(segments.clone(), 5), segments);
    }

    #[test]
    fn cap_speakers_keeps_the_most_talkative_and_reassigns_by_proximity() {
        // Speaker 0: 4 s, speaker 1: 3 s, speaker 2: 1 s, speaker 3: 0.5 s.
        let segments = vec![
            segment(0.0, 4.0, 0),
            segment(4.5, 5.5, 2), // 0.5 s after speaker 0, 2.5 s before speaker 1
            segment(8.0, 11.0, 1),
            segment(11.2, 11.7, 3), // 0.2 s after speaker 1
        ];
        let capped = cap_speakers(segments, 2);
        assert_eq!(
            capped,
            vec![
                segment(0.0, 4.0, 0),
                segment(4.5, 5.5, 0),
                segment(8.0, 11.0, 1),
                segment(11.2, 11.7, 1),
            ]
        );
    }

    #[test]
    fn cap_speakers_merges_same_speaker_overlaps() {
        let segments = vec![
            segment(0.0, 3.0, 0),
            segment(2.0, 4.0, 1), // overlaps speaker 0, so it joins speaker 0
            segment(10.0, 12.0, 2),
            segment(12.5, 13.0, 2),
        ];
        let capped = cap_speakers(segments, 2);
        assert_eq!(
            capped,
            vec![
                segment(0.0, 4.0, 0),
                segment(10.0, 12.0, 2),
                segment(12.5, 13.0, 2),
            ]
        );
    }

    #[test]
    fn cap_speakers_to_one_collapses_everyone_and_sorts_by_start() {
        let segments = vec![
            segment(0.0, 1.0, 1),
            segment(2.0, 5.0, 0),
            segment(6.0, 7.0, 2),
        ];
        let capped = cap_speakers(segments, 1);
        assert!(capped.iter().all(|segment| segment.speaker == 0));
        let starts: Vec<f64> = capped.iter().map(|segment| segment.start).collect();
        assert_eq!(starts, vec![0.0, 2.0, 6.0]);
    }

    #[test]
    fn cap_speakers_breaks_distance_ties_toward_more_speech() {
        let segments = vec![
            segment(0.0, 2.0, 0), // 2 s
            segment(3.0, 4.0, 2), // 1 s from both kept speakers
            segment(5.0, 6.0, 1), // 1 s
            segment(20.0, 20.2, 3),
        ];
        let capped = cap_speakers(segments, 2);
        assert_eq!(capped[1], segment(3.0, 4.0, 0));
    }

    #[test]
    fn segmenting_percentages_stay_in_their_band() {
        assert_eq!(segmenting_stage_percentage(0.0), 40);
        assert_eq!(segmenting_stage_percentage(0.5), 55);
        assert_eq!(segmenting_stage_percentage(1.0), 69);
        assert_eq!(segmenting_stage_percentage(-1.0), 40);
        assert_eq!(segmenting_stage_percentage(f32::NAN), 40);
    }

    #[test]
    fn engine_metadata_names_fluidaudio() {
        assert_eq!(FLUIDAUDIO_ENGINE.engine, "fluidaudio-0.18.0");
        assert_eq!(
            FLUIDAUDIO_ENGINE.segmentation_model,
            "pyannote-segmentation-community-1"
        );
        assert_eq!(FLUIDAUDIO_ENGINE.embedding_model, "wespeaker-v2");
    }

    /// Runs this module's diarizer path (helper, speaker cap, turn mapping,
    /// label stabilisation) on a real two-speaker clip:
    /// `MINUTES_SPEECH_HELPER=... MINUTES_SPEECH_MODELS=...
    /// MEETILY_DIARIZATION_FIXTURE=... cargo test -p meetily --lib
    /// audio::diarization -- --ignored`.
    #[tokio::test]
    #[ignore = "needs MINUTES_SPEECH_HELPER, MINUTES_SPEECH_MODELS and MEETILY_DIARIZATION_FIXTURE"]
    async fn fluidaudio_diarizes_two_speaker_fixture() {
        let models = std::env::var("MINUTES_SPEECH_MODELS")
            .expect("MINUTES_SPEECH_MODELS must point to the FluidAudio models directory");
        let fixture = std::env::var("MEETILY_DIARIZATION_FIXTURE")
            .expect("MEETILY_DIARIZATION_FIXTURE must point to a 16 kHz WAV file");
        speech::batch::configure(PathBuf::from(models));
        let samples = decode_audio_file(Path::new(&fixture))
            .expect("fixture should decode")
            .to_whisper_format();
        let segments = from_helper(
            speech::batch::diarize(&samples, Some(2), None)
                .await
                .expect("fixture should diarize"),
        );
        speech::batch::unload().await;

        let mut turns = segments_to_turns(cap_speakers(segments.clone(), 2));
        stabilize_turn_labels(&mut turns, &[], &HashSet::new());
        let speakers = turns
            .iter()
            .map(|turn| turn.speaker.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(speakers, HashSet::from(["speaker_00", "speaker_01"]));
        assert!(turns
            .iter()
            .all(|turn| turn.start >= 0.0 && turn.end > turn.start));

        let single = cap_speakers(segments, 1);
        let single_speakers = single
            .iter()
            .map(|segment| segment.speaker)
            .collect::<HashSet<_>>();
        assert_eq!(single_speakers.len(), 1);
    }
}
