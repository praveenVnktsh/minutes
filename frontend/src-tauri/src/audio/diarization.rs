use crate::api::TranscriptSegment;
use crate::audio::common::write_transcripts_json;
use crate::audio::decoder::decode_audio_file;
use crate::audio::retranscription::find_audio_file;
use crate::database::models::Transcript;
use crate::state::AppState;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

const ENGINE: &str = "sherpa-onnx-1.13.8";
const SEGMENTATION_MODEL: &str = "pyannote-segmentation-3.0";
const EMBEDDING_MODEL: &str = "wespeaker-en-voxceleb-resnet34-lm";
const SEGMENTATION_DIR: &str = "sherpa-onnx-pyannote-segmentation-3-0";
// fp32 segmentation detects speaker boundaries more accurately than the int8
// build; the extra ~4 MB on disk is a worthwhile trade for a local app.
const SEGMENTATION_FILE: &str = "model.onnx";
const SEGMENTATION_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
// VoxCeleb ResNet34 with large-margin training. English-focused and a much
// stronger speaker discriminator than NeMo titanet-small, at a smaller size.
const EMBEDDING_FILE: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";
const EMBEDDING_MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx";
// Fast clustering merges speakers above this cosine threshold. Override at
// runtime with MEETILY_DIARIZATION_THRESHOLD to tune without rebuilding.
const DEFAULT_CLUSTER_THRESHOLD: f32 = 0.8;

fn cluster_threshold() -> f32 {
    std::env::var("MEETILY_DIARIZATION_THRESHOLD")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| *value > 0.0 && *value <= 1.0)
        .unwrap_or(DEFAULT_CLUSTER_THRESHOLD)
}

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

struct ModelPaths {
    segmentation: PathBuf,
    embedding: PathBuf,
}

#[cfg(target_os = "macos")]
async fn download(url: &str, destination: &Path) -> Result<()> {
    if destination.exists() && destination.metadata()?.len() > 0 {
        return Ok(());
    }
    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;
    let partial = destination.with_extension("part");
    tokio::fs::write(&partial, &bytes).await?;
    tokio::fs::rename(&partial, destination).await?;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn ensure_models<R: Runtime>(app: &AppHandle<R>, meeting_id: &str) -> Result<ModelPaths> {
    let root = app
        .path()
        .app_data_dir()?
        .join("models")
        .join("diarization");
    tokio::fs::create_dir_all(&root).await?;

    let segmentation_dir = root.join(SEGMENTATION_DIR);
    let segmentation = segmentation_dir.join(SEGMENTATION_FILE);
    if !segmentation.exists() {
        let archive_path = root.join("segmentation.tar.bz2");
        emit_stage(
            app,
            meeting_id,
            "downloading_models",
            5,
            "Downloading local speaker segmentation model...",
        );
        download(SEGMENTATION_ARCHIVE_URL, &archive_path).await?;
        let root_for_extract = root.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = std::fs::File::open(&archive_path)?;
            let decoder = bzip2::read::BzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(&root_for_extract)?;
            std::fs::remove_file(archive_path)?;
            Ok(())
        })
        .await
        .map_err(|error| anyhow!("Model extraction task failed: {error}"))??;
    }

    let embedding = root.join(EMBEDDING_FILE);
    if !embedding.exists() {
        emit_stage(
            app,
            meeting_id,
            "downloading_models",
            10,
            "Downloading local speaker embedding model...",
        );
        download(EMBEDDING_MODEL_URL, &embedding).await?;
    }

    if !segmentation.exists() || !embedding.exists() {
        return Err(anyhow!("Diarization models are incomplete after download"));
    }
    Ok(ModelPaths {
        segmentation,
        embedding,
    })
}

#[cfg(not(target_os = "macos"))]
async fn ensure_models<R: Runtime>(_app: &AppHandle<R>, _meeting_id: &str) -> Result<ModelPaths> {
    Err(anyhow!(
        "Local speaker diarization is currently supported on macOS"
    ))
}

#[cfg(target_os = "macos")]
fn diarize_samples(
    samples: &[f32],
    models: &ModelPaths,
    num_speakers: Option<usize>,
) -> Result<Vec<SpeakerTurn>> {
    use sherpa_onnx::{
        FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
        OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
        SpeakerEmbeddingExtractorConfig,
    };

    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(models.segmentation.to_string_lossy().into_owned()),
                window_shift_ratio: 0.1,
            },
            num_threads: 2,
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(models.embedding.to_string_lossy().into_owned()),
            num_threads: 2,
            ..Default::default()
        },
        clustering: FastClusteringConfig {
            num_clusters: num_speakers.map(|value| value as i32).unwrap_or(-1),
            threshold: cluster_threshold(),
            compute_confidence: true,
        },
        ..Default::default()
    };
    let diarizer = OfflineSpeakerDiarization::create(&config)
        .ok_or_else(|| anyhow!("Failed to initialize the local diarization engine"))?;
    if diarizer.sample_rate() != 16_000 {
        return Err(anyhow!(
            "Diarization model expected {}Hz audio",
            diarizer.sample_rate()
        ));
    }
    let result = diarizer
        .process(samples)
        .ok_or_else(|| anyhow!("Local diarization returned no result"))?;
    Ok(result
        .sort_by_start_time()
        .into_iter()
        .map(|turn| SpeakerTurn {
            start: turn.start as f64,
            end: turn.end as f64,
            speaker: format!("raw_{:02}", turn.speaker),
            confidence: turn.confidence,
        })
        .collect())
}

#[cfg(not(target_os = "macos"))]
fn diarize_samples(
    _samples: &[f32],
    _models: &ModelPaths,
    _num_speakers: Option<usize>,
) -> Result<Vec<SpeakerTurn>> {
    Err(anyhow!(
        "Local speaker diarization is currently supported on macOS"
    ))
}

fn overlap(start_a: f64, end_a: f64, start_b: f64, end_b: f64) -> f64 {
    (end_a.min(end_b) - start_a.max(start_b)).max(0.0)
}

/// Reuse existing speaker IDs when a rerun overlaps previously labeled transcript
/// segments, then allocate deterministic IDs to any newly discovered clusters.
fn stabilize_turn_labels(turns: &mut [SpeakerTurn], transcripts: &[Transcript]) {
    let mut scores: HashMap<(String, String), f64> = HashMap::new();
    for turn in turns.iter() {
        for transcript in transcripts {
            let Some(previous) = transcript
                .speaker
                .as_deref()
                .filter(|value| value.starts_with("speaker_"))
            else {
                continue;
            };
            let (Some(start), Some(end)) = (transcript.audio_start_time, transcript.audio_end_time)
            else {
                continue;
            };
            let amount = overlap(turn.start, turn.end, start, end);
            if amount > 0.0 {
                *scores
                    .entry((turn.speaker.clone(), previous.to_string()))
                    .or_default() += amount;
            }
        }
    }

    let mut candidates: Vec<_> = scores.into_iter().collect();
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut mapping = HashMap::new();
    let mut used_labels = HashSet::new();
    for ((raw, previous), _) in candidates {
        if !mapping.contains_key(&raw) && used_labels.insert(previous.clone()) {
            mapping.insert(raw, previous);
        }
    }

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

pub async fn run_for_meeting<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    num_speakers: Option<usize>,
) -> Result<DiarizationResult> {
    if matches!(num_speakers, Some(0)) {
        return Err(anyhow!("num_speakers must be greater than zero"));
    }
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
    let models = ensure_models(app, meeting_id).await?;
    emit_stage(
        app,
        meeting_id,
        "decoding_audio",
        20,
        "Decoding meeting audio for speaker analysis...",
    );
    let samples = tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
        Ok(decode_audio_file(&audio_path)?.to_whisper_format())
    })
    .await
    .map_err(|error| anyhow!("Audio decode task failed: {error}"))??;
    // Sherpa's offline pipeline runs segmentation, embedding extraction and
    // clustering in one call with no progress callback, so this covers the bulk
    // of the wait and the stage label stands in for all three.
    emit_stage(
        app,
        meeting_id,
        "segmenting",
        40,
        "Detecting and clustering speakers...",
    );
    let mut turns =
        tokio::task::spawn_blocking(move || diarize_samples(&samples, &models, num_speakers))
            .await
            .map_err(|error| anyhow!("Diarization task failed: {error}"))??;
    if turns.is_empty() {
        return Err(anyhow!("No speaker turns were detected"));
    }
    emit_stage(
        app,
        meeting_id,
        "clustering",
        70,
        "Grouping speaker turns...",
    );
    stabilize_turn_labels(&mut turns, &transcripts);
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
        engine: ENGINE.to_string(),
        segmentation_model: SEGMENTATION_MODEL.to_string(),
        embedding_model: EMBEDDING_MODEL.to_string(),
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

#[tauri::command]
pub async fn run_speaker_diarization<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    num_speakers: Option<usize>,
) -> Result<DiarizationResult, String> {
    run_for_meeting(&app, &meeting_id, num_speakers)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        stabilize_turn_labels(&mut turns, &transcripts);
        assert_eq!(turns[0].speaker, "speaker_03");
        assert_eq!(turns[1].speaker, "speaker_01");
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

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires downloaded models and MEETILY_DIARIZATION_FIXTURE"]
    fn diarizes_real_multi_speaker_fixture() {
        let fixture = std::env::var("MEETILY_DIARIZATION_FIXTURE")
            .expect("MEETILY_DIARIZATION_FIXTURE must point to a 16 kHz WAV file");
        let segmentation = std::env::var("MEETILY_SEGMENTATION_MODEL")
            .expect("MEETILY_SEGMENTATION_MODEL must point to model.int8.onnx");
        let embedding = std::env::var("MEETILY_EMBEDDING_MODEL")
            .expect("MEETILY_EMBEDDING_MODEL must point to an embedding ONNX model");
        let audio = decode_audio_file(Path::new(&fixture)).expect("fixture should decode");
        let turns = diarize_samples(
            &audio.to_whisper_format(),
            &ModelPaths {
                segmentation: segmentation.into(),
                embedding: embedding.into(),
            },
            Some(4),
        )
        .expect("fixture should diarize");

        let speakers = turns
            .iter()
            .map(|turn| turn.speaker.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(speakers.len(), 4);
        assert!(turns
            .iter()
            .all(|turn| turn.start >= 0.0 && turn.end > turn.start));
    }
}
