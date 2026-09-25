//! Offline, local speaker diarization for completed recordings.
//!
//! The default engine on every platform is NVIDIA's Nemotron-3 streaming
//! Sortformer ([`crate::audio::sortformer`]), run through ONNX Runtime. On macOS
//! the older sherpa-onnx pipeline (pyannote segmentation + WeSpeaker embeddings +
//! clustering) is kept as a fallback when Nemotron fails, and is used directly
//! when the caller asks for a fixed speaker count, which only its clustering can
//! honour.

use crate::api::TranscriptSegment;
use crate::audio::common::write_transcripts_json;
use crate::audio::decoder::decode_audio_file;
use crate::audio::retranscription::find_audio_file;
use crate::audio::sortformer::{self, SortformerDiarizer, SpeakerSegment};
use crate::database::models::Transcript;
use crate::state::AppState;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Names recorded with a diarization run so the saved result says which engine
/// and models actually produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EngineInfo {
    engine: &'static str,
    segmentation_model: &'static str,
    embedding_model: &'static str,
}

/// Nemotron-3 has no separate embedding model: speaker identity across chunks
/// comes from its arrival-order speaker cache (AOSC).
const NEMOTRON_ENGINE: EngineInfo = EngineInfo {
    engine: "onnxruntime-nemotron-3",
    segmentation_model: "nemotron-3-diarization-offline",
    embedding_model: "sortformer-aosc",
};

#[cfg(target_os = "macos")]
const PYANNOTE_ENGINE: EngineInfo = EngineInfo {
    engine: "sherpa-onnx-1.13.8",
    segmentation_model: "pyannote-segmentation-3.0",
    embedding_model: "wespeaker-en-voxceleb-resnet34-lm",
};

/// Directory under `models/diarization` holding the Nemotron-3 model.
const NEMOTRON_DIR: &str = "nemotron-3";
/// Give up on a stalled model download after this long without data.
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(target_os = "macos")]
const PYANNOTE_SEGMENTATION_DIR: &str = "sherpa-onnx-pyannote-segmentation-3-0";
// fp32 segmentation detects speaker boundaries more accurately than the int8
// build; the extra ~4 MB on disk is a worthwhile trade for a local app.
#[cfg(target_os = "macos")]
const PYANNOTE_SEGMENTATION_FILE: &str = "model.onnx";
#[cfg(target_os = "macos")]
const PYANNOTE_SEGMENTATION_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
// VoxCeleb ResNet34 with large-margin training. English-focused and a much
// stronger speaker discriminator than NeMo titanet-small, at a smaller size.
#[cfg(target_os = "macos")]
const PYANNOTE_EMBEDDING_FILE: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";
#[cfg(target_os = "macos")]
const PYANNOTE_EMBEDDING_MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx";
// Fast clustering merges speakers above this cosine threshold. Override at
// runtime with MEETILY_DIARIZATION_THRESHOLD to tune without rebuilding.
#[cfg(target_os = "macos")]
const DEFAULT_CLUSTER_THRESHOLD: f32 = 0.8;

#[cfg(target_os = "macos")]
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

// ---------------------------------------------------------------------------
// Nemotron-3 (default engine)
// ---------------------------------------------------------------------------

/// Overall progress for a model download that is `percent` complete. Downloads
/// report inside 5..=19 %, before audio decoding starts at 20 %.
fn download_stage_percentage(percent: u32) -> u32 {
    5 + percent.min(100) * 14 / 100
}

/// Overall progress for a Sortformer run that has scored `fraction` of the
/// audio, inside the 40..70 % "segmenting" band.
fn segmenting_stage_percentage(fraction: f32) -> u32 {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    40 + (fraction * 29.0).round() as u32
}

/// Stream `url` into `destination`, checking its size and SHA-256 before it is
/// moved into place. The body is written to `<destination>.part` as it arrives
/// (the model is ~400 MB, too big to buffer) and the partial file is removed on
/// any failure, so `destination` only ever exists fully verified.
/// `on_percent` is called each time the whole-number percentage changes.
async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
    on_percent: impl FnMut(u32),
) -> Result<()> {
    let partial = destination.with_extension("part");
    let outcome = stream_to_file(
        client,
        url,
        &partial,
        destination,
        expected_bytes,
        expected_sha256,
        on_percent,
    )
    .await;
    if outcome.is_err() {
        if let Err(error) = tokio::fs::remove_file(&partial).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "Diarization: could not remove partial download {}: {}",
                    partial.display(),
                    error
                );
            }
        }
    }
    outcome
}

async fn stream_to_file(
    client: &reqwest::Client,
    url: &str,
    partial: &Path,
    destination: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
    mut on_percent: impl FnMut(u32),
) -> Result<()> {
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to request {url}"))?
        .error_for_status()?;
    let file = tokio::fs::File::create(partial)
        .await
        .with_context(|| format!("Failed to create {}", partial.display()))?;
    let mut writer = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut last_percent = None;
    loop {
        let chunk = tokio::time::timeout(DOWNLOAD_READ_TIMEOUT, response.chunk())
            .await
            .map_err(|_| {
                anyhow!(
                    "Download stalled: no data for {}s",
                    DOWNLOAD_READ_TIMEOUT.as_secs()
                )
            })??;
        let Some(chunk) = chunk else { break };
        received += chunk.len() as u64;
        if received > expected_bytes {
            bail!("Download is larger than the expected {expected_bytes} bytes");
        }
        hasher.update(&chunk);
        writer.write_all(&chunk).await?;
        let percent = (received * 100 / expected_bytes.max(1)) as u32;
        if last_percent != Some(percent) {
            last_percent = Some(percent);
            on_percent(percent);
        }
    }
    writer.flush().await?;
    writer.get_ref().sync_all().await?;
    drop(writer);

    if received != expected_bytes {
        bail!("Download is incomplete: received {received} of {expected_bytes} bytes");
    }
    let digest = hex::encode(hasher.finalize());
    if !digest.eq_ignore_ascii_case(expected_sha256) {
        bail!("Download checksum mismatch: expected {expected_sha256}, got {digest}");
    }
    tokio::fs::rename(partial, destination)
        .await
        .with_context(|| format!("Failed to move model into {}", destination.display()))?;
    Ok(())
}

/// Serialises Nemotron model downloads so two meetings diarizing at once do not
/// write the same `.part` file.
static NEMOTRON_DOWNLOAD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Make sure the Nemotron-3 model is at
/// `app_data_dir/models/diarization/nemotron-3/<MODEL_FILE>`, downloading it if
/// needed. A file of the expected size is trusted as-is: it only gets there
/// after its checksum is verified, so re-hashing 400 MB on every run is skipped.
async fn ensure_nemotron_model<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()?
        .join("models")
        .join("diarization")
        .join(NEMOTRON_DIR);
    let model = dir.join(sortformer::MODEL_FILE);

    let _guard = NEMOTRON_DOWNLOAD_LOCK.lock().await;
    match tokio::fs::metadata(&model).await {
        Ok(metadata) if metadata.len() == sortformer::MODEL_BYTES => return Ok(model),
        Ok(metadata) => {
            log::warn!(
                "Diarization: {} has {} bytes, expected {}; downloading it again",
                model.display(),
                metadata.len(),
                sortformer::MODEL_BYTES
            );
            tokio::fs::remove_file(&model).await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::create_dir_all(&dir).await?;

    log::info!(
        "Diarization: downloading Nemotron-3 model ({} bytes) to {}",
        sortformer::MODEL_BYTES,
        model.display()
    );
    emit_stage(
        app,
        meeting_id,
        "downloading_models",
        download_stage_percentage(0),
        "Downloading Nemotron-3 speaker model...",
    );
    let client = reqwest::Client::builder()
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .build()?;
    download_verified(
        &client,
        sortformer::MODEL_URL,
        &model,
        sortformer::MODEL_BYTES,
        sortformer::MODEL_SHA256,
        |percent| {
            emit_stage(
                app,
                meeting_id,
                "downloading_models",
                download_stage_percentage(percent),
                &format!("Downloading Nemotron-3 speaker model... {percent}%"),
            )
        },
    )
    .await
    .context("Failed to download the Nemotron-3 speaker model")?;
    log::info!("Diarization: Nemotron-3 model downloaded and verified");
    Ok(model)
}

/// Keep at most `max_speakers` speakers, for callers that know how many people
/// were in the meeting. Sortformer has no clustering step to steer, so the hint
/// is applied afterwards: the `max_speakers` speakers with the most total speech
/// are kept, and each segment of any other speaker is handed to the kept
/// speaker whose speech is closest in time to it (gap between segment edges;
/// overlapping speech counts as distance zero; ties go to the speaker with more
/// speech). Surplus speakers are usually one person split in two around their
/// own turns, so the nearest kept speaker is the most plausible owner.
/// Same-speaker segments that end up overlapping or touching are merged, with a
/// duration-weighted confidence. The result is sorted by start, then speaker.
fn cap_speakers(segments: Vec<SpeakerSegment>, max_speakers: usize) -> Vec<SpeakerSegment> {
    let mut totals: HashMap<usize, f64> = HashMap::new();
    for segment in &segments {
        *totals.entry(segment.speaker).or_default() += segment.end - segment.start;
    }
    if totals.len() <= max_speakers || max_speakers == 0 {
        return segments;
    }
    let mut ranked: Vec<(usize, f64)> = totals.iter().map(|(&k, &v)| (k, v)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let kept: Vec<usize> = ranked
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
                let last_duration = last.end - last.start;
                let duration = segment.end - segment.start;
                let total = last_duration + duration;
                if total > 0.0 {
                    last.confidence = ((last.confidence as f64 * last_duration
                        + segment.confidence as f64 * duration)
                        / total) as f32;
                }
                last.end = last.end.max(segment.end);
            }
            _ => merged.push(segment),
        }
    }
    merged.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.speaker.cmp(&b.speaker)));
    merged
}

/// Convert Sortformer segments to turns, labelled `raw_NN` like the pyannote
/// engine's clusters so label stabilisation and persistence treat both alike.
fn segments_to_turns(segments: Vec<SpeakerSegment>) -> Vec<SpeakerTurn> {
    segments
        .into_iter()
        .map(|segment| SpeakerTurn {
            start: segment.start,
            end: segment.end,
            speaker: format!("raw_{:02}", segment.speaker),
            confidence: segment.confidence,
        })
        .collect()
}

/// Run Nemotron-3 over the meeting audio: model ensure, load, then diarize off
/// the async runtime with per-step progress.
async fn run_nemotron<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    audio: &mut MeetingAudio,
    num_speakers: Option<usize>,
) -> Result<Vec<SpeakerTurn>> {
    let model = ensure_nemotron_model(app, meeting_id).await?;
    let samples = audio.samples(app, meeting_id).await?;
    emit_stage(
        app,
        meeting_id,
        "segmenting",
        segmenting_stage_percentage(0.0),
        "Detecting speakers...",
    );
    let progress_app = app.clone();
    let progress_meeting = meeting_id.to_string();
    let segments = tokio::task::spawn_blocking(move || -> Result<Vec<SpeakerSegment>> {
        let mut diarizer = SortformerDiarizer::new(&model)?;
        let mut last = None;
        diarizer.diarize(&samples, |fraction| {
            let percentage = segmenting_stage_percentage(fraction);
            if last != Some(percentage) {
                last = Some(percentage);
                emit_stage(
                    &progress_app,
                    &progress_meeting,
                    "segmenting",
                    percentage,
                    "Detecting speakers...",
                );
            }
        })
    })
    .await
    .map_err(|error| anyhow!("Diarization task failed: {error}"))??;

    let segments = match num_speakers {
        Some(limit) => {
            let before = segments
                .iter()
                .map(|segment| segment.speaker)
                .collect::<HashSet<_>>()
                .len();
            if before > limit {
                log::info!(
                    "Diarization: Nemotron-3 found {} speakers; merging down to the requested {}",
                    before,
                    limit
                );
            }
            cap_speakers(segments, limit)
        }
        None => segments,
    };
    Ok(segments_to_turns(segments))
}

// ---------------------------------------------------------------------------
// pyannote + WeSpeaker via sherpa-onnx (macOS fallback)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
struct PyannoteModels {
    segmentation: PathBuf,
    embedding: PathBuf,
}

/// Fetch one of the small pyannote/WeSpeaker files (a few MB), buffered.
#[cfg(target_os = "macos")]
async fn download_small(url: &str, destination: &Path) -> Result<()> {
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
async fn ensure_pyannote_models<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
) -> Result<PyannoteModels> {
    let root = app
        .path()
        .app_data_dir()?
        .join("models")
        .join("diarization");
    tokio::fs::create_dir_all(&root).await?;

    let segmentation_dir = root.join(PYANNOTE_SEGMENTATION_DIR);
    let segmentation = segmentation_dir.join(PYANNOTE_SEGMENTATION_FILE);
    if !segmentation.exists() {
        let archive_path = root.join("segmentation.tar.bz2");
        emit_stage(
            app,
            meeting_id,
            "downloading_models",
            5,
            "Downloading local speaker segmentation model...",
        );
        download_small(PYANNOTE_SEGMENTATION_ARCHIVE_URL, &archive_path).await?;
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

    let embedding = root.join(PYANNOTE_EMBEDDING_FILE);
    if !embedding.exists() {
        emit_stage(
            app,
            meeting_id,
            "downloading_models",
            10,
            "Downloading local speaker embedding model...",
        );
        download_small(PYANNOTE_EMBEDDING_MODEL_URL, &embedding).await?;
    }

    if !segmentation.exists() || !embedding.exists() {
        return Err(anyhow!("Diarization models are incomplete after download"));
    }
    Ok(PyannoteModels {
        segmentation,
        embedding,
    })
}

#[cfg(target_os = "macos")]
fn diarize_with_pyannote(
    samples: &[f32],
    models: &PyannoteModels,
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

/// Run the sherpa-onnx pyannote pipeline over the meeting audio.
#[cfg(target_os = "macos")]
async fn run_pyannote<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    audio: &mut MeetingAudio,
    num_speakers: Option<usize>,
) -> Result<Vec<SpeakerTurn>> {
    let models = ensure_pyannote_models(app, meeting_id).await?;
    let samples = audio.samples(app, meeting_id).await?;
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
    tokio::task::spawn_blocking(move || diarize_with_pyannote(&samples, &models, num_speakers))
        .await
        .map_err(|error| anyhow!("Diarization task failed: {error}"))?
}

// ---------------------------------------------------------------------------
// Engine selection
// ---------------------------------------------------------------------------

/// Meeting audio decoded to 16 kHz mono on first use and then shared, so a
/// fallback to the second engine does not decode the recording again.
struct MeetingAudio {
    path: PathBuf,
    samples: Option<Arc<Vec<f32>>>,
}

impl MeetingAudio {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            samples: None,
        }
    }

    async fn samples<R: Runtime>(
        &mut self,
        app: &AppHandle<R>,
        meeting_id: &str,
    ) -> Result<Arc<Vec<f32>>> {
        if let Some(samples) = &self.samples {
            return Ok(Arc::clone(samples));
        }
        emit_stage(
            app,
            meeting_id,
            "decoding_audio",
            20,
            "Decoding meeting audio for speaker analysis...",
        );
        let path = self.path.clone();
        let samples = tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
            Ok(decode_audio_file(&path)?.to_whisper_format())
        })
        .await
        .map_err(|error| anyhow!("Audio decode task failed: {error}"))??;
        let samples = Arc::new(samples);
        self.samples = Some(Arc::clone(&samples));
        Ok(samples)
    }
}

/// Diarize the meeting audio with the best available engine and report which
/// one produced the turns.
async fn diarize_meeting_audio<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    audio_path: PathBuf,
    num_speakers: Option<usize>,
) -> Result<(Vec<SpeakerTurn>, EngineInfo)> {
    let mut audio = MeetingAudio::new(audio_path);

    // pyannote's clustering can target an exact speaker count; Sortformer can
    // only be capped afterwards, so prefer pyannote when a count is given.
    #[cfg(target_os = "macos")]
    if num_speakers.is_some() {
        log::info!("Diarization: speaker count given, using the pyannote engine");
        let turns = run_pyannote(app, meeting_id, &mut audio, num_speakers).await?;
        return Ok((turns, PYANNOTE_ENGINE));
    }

    match run_nemotron(app, meeting_id, &mut audio, num_speakers).await {
        Ok(turns) => Ok((turns, NEMOTRON_ENGINE)),
        #[cfg(target_os = "macos")]
        Err(error) => {
            log::warn!(
                "Diarization: Nemotron-3 failed, falling back to pyannote: {:#}",
                error
            );
            let turns = run_pyannote(app, meeting_id, &mut audio, num_speakers).await?;
            Ok((turns, PYANNOTE_ENGINE))
        }
        #[cfg(not(target_os = "macos"))]
        Err(error) => Err(error.context("Nemotron-3 speaker diarization failed")),
    }
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

/// Meetings being diarized in this process. `diarization_state` alone cannot
/// tell a running meeting from one whose run died with the app, so the status
/// command checks this first.
static RUNNING: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

/// Holds a meeting's slot in [`RUNNING`] and frees it on drop, so an early
/// return or a panic cannot leave the meeting stuck as running.
struct RunningSlot(String);

impl RunningSlot {
    fn acquire(meeting_id: &str) -> Result<Self> {
        let mut running = RUNNING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !running.get_or_insert_with(HashSet::new).insert(meeting_id.to_string()) {
            bail!("Speaker identification is already running for this meeting");
        }
        Ok(Self(meeting_id.to_string()))
    }
}

impl Drop for RunningSlot {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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

async fn diarization_status(pool: &sqlx::SqlitePool, meeting_id: &str) -> Result<DiarizationStatus> {
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
            match run_for_meeting(&app, &meeting_id, None).await {
                Ok(result) => log::info!(
                    "Diarization resume: meeting {meeting_id} has {} speaker(s)",
                    result.speaker_count
                ),
                // run_for_meeting already logged and recorded the failure.
                Err(_) => {}
            }
        }
    });
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
        assert_eq!(diarization_status(&pool, "m1").await.unwrap(), DiarizationStatus::Missing);

        record_started(&pool, "m1").await.unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Pending { attempts: 1 },
            "a started run with no live slot reads as interrupted"
        );
        {
            let _slot = RunningSlot::acquire("m1").unwrap();
            assert_eq!(diarization_status(&pool, "m1").await.unwrap(), DiarizationStatus::Running);
        }

        record_finished(&pool, "m1", Some("No speaker turns were detected")).await.unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Failed { error: "No speaker turns were detected".into() }
        );

        record_started(&pool, "m1").await.unwrap();
        assert_eq!(
            diarization_status(&pool, "m1").await.unwrap(),
            DiarizationStatus::Pending { attempts: 2 }
        );
        record_finished(&pool, "m1", None).await.unwrap();
        assert_eq!(diarization_status(&pool, "m1").await.unwrap(), DiarizationStatus::Done);

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
        assert_eq!(diarization_status(&pool, "m2").await.unwrap(), DiarizationStatus::Done);
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
        assert_eq!(interrupted_meetings(&pool).await.unwrap(), vec!["m1".to_string()]);

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

    fn segment(start: f64, end: f64, speaker: usize, confidence: f32) -> SpeakerSegment {
        SpeakerSegment {
            start,
            end,
            speaker,
            confidence,
        }
    }

    #[test]
    fn segments_map_to_raw_labelled_turns() {
        let turns = segments_to_turns(vec![segment(0.5, 1.25, 0, 0.9), segment(1.0, 3.0, 7, 0.6)]);
        assert_eq!(
            turns,
            vec![
                SpeakerTurn {
                    start: 0.5,
                    end: 1.25,
                    speaker: "raw_00".into(),
                    confidence: 0.9,
                },
                SpeakerTurn {
                    start: 1.0,
                    end: 3.0,
                    speaker: "raw_07".into(),
                    confidence: 0.6,
                },
            ]
        );
    }

    #[test]
    fn nemotron_turns_feed_label_stabilisation() {
        let mut turns =
            segments_to_turns(vec![segment(0.0, 2.0, 1, 0.9), segment(2.0, 4.0, 0, 0.9)]);
        stabilize_turn_labels(&mut turns, &[]);
        assert_eq!(turns[0].speaker, "speaker_01");
        assert_eq!(turns[1].speaker, "speaker_00");
    }

    #[test]
    fn cap_speakers_is_a_no_op_within_the_limit() {
        let segments = vec![segment(0.0, 1.0, 0, 0.9), segment(1.0, 2.0, 1, 0.8)];
        assert_eq!(cap_speakers(segments.clone(), 2), segments);
        assert_eq!(cap_speakers(segments.clone(), 5), segments);
    }

    #[test]
    fn cap_speakers_keeps_the_most_talkative_and_reassigns_by_proximity() {
        // Speaker 0: 4 s, speaker 1: 3 s, speaker 2: 1 s, speaker 3: 0.5 s.
        let segments = vec![
            segment(0.0, 4.0, 0, 0.9),
            segment(4.5, 5.5, 2, 0.5), // 0.5 s after speaker 0, 2.5 s before speaker 1
            segment(8.0, 11.0, 1, 0.8),
            segment(11.2, 11.7, 3, 0.4), // 0.2 s after speaker 1
        ];
        let capped = cap_speakers(segments, 2);
        assert_eq!(
            capped,
            vec![
                segment(0.0, 4.0, 0, 0.9),
                segment(4.5, 5.5, 0, 0.5),
                segment(8.0, 11.0, 1, 0.8),
                segment(11.2, 11.7, 1, 0.4),
            ]
        );
    }

    #[test]
    fn cap_speakers_merges_same_speaker_overlaps_with_weighted_confidence() {
        let segments = vec![
            segment(0.0, 3.0, 0, 1.0),
            segment(2.0, 4.0, 1, 0.5), // overlaps speaker 0, so it joins speaker 0
            segment(10.0, 12.0, 2, 0.7),
            segment(12.5, 13.0, 2, 0.7),
        ];
        let capped = cap_speakers(segments, 2);
        assert_eq!(capped.len(), 3);
        assert_eq!(capped[0].speaker, 0);
        assert_eq!((capped[0].start, capped[0].end), (0.0, 4.0));
        // (1.0 * 3 s + 0.5 * 2 s) / 5 s
        assert!((capped[0].confidence - 0.8).abs() < 1e-6);
        assert_eq!(capped[1], segment(10.0, 12.0, 2, 0.7));
        assert_eq!(capped[2], segment(12.5, 13.0, 2, 0.7));
    }

    #[test]
    fn cap_speakers_to_one_collapses_everyone_and_sorts_by_start() {
        let segments = vec![
            segment(0.0, 1.0, 1, 0.9),
            segment(2.0, 5.0, 0, 0.9),
            segment(6.0, 7.0, 2, 0.9),
        ];
        let capped = cap_speakers(segments, 1);
        assert!(capped.iter().all(|segment| segment.speaker == 0));
        let starts: Vec<f64> = capped.iter().map(|segment| segment.start).collect();
        assert_eq!(starts, vec![0.0, 2.0, 6.0]);
    }

    #[test]
    fn cap_speakers_breaks_distance_ties_toward_more_speech() {
        let segments = vec![
            segment(0.0, 2.0, 0, 0.9), // 2 s
            segment(3.0, 4.0, 2, 0.9), // 1 s from both kept speakers
            segment(5.0, 6.0, 1, 0.9), // 1 s
            segment(20.0, 20.2, 3, 0.9),
        ];
        let capped = cap_speakers(segments, 2);
        assert_eq!(capped[1], segment(3.0, 4.0, 0, 0.9));
    }

    #[test]
    fn stage_percentages_stay_in_their_bands() {
        assert_eq!(download_stage_percentage(0), 5);
        assert_eq!(download_stage_percentage(42), 10);
        assert_eq!(download_stage_percentage(100), 19);
        assert_eq!(download_stage_percentage(250), 19);
        assert_eq!(segmenting_stage_percentage(0.0), 40);
        assert_eq!(segmenting_stage_percentage(0.5), 55);
        assert_eq!(segmenting_stage_percentage(1.0), 69);
        assert_eq!(segmenting_stage_percentage(-1.0), 40);
        assert_eq!(segmenting_stage_percentage(f32::NAN), 40);
    }

    #[test]
    fn nemotron_metadata_names_the_engine() {
        assert_eq!(NEMOTRON_ENGINE.engine, "onnxruntime-nemotron-3");
        assert_eq!(
            NEMOTRON_ENGINE.segmentation_model,
            "nemotron-3-diarization-offline"
        );
        assert_eq!(NEMOTRON_ENGINE.embedding_model, "sortformer-aosc");
    }

    /// Serve `body` once over plain HTTP on localhost and return its URL.
    async fn serve_once(body: Vec<u8>) -> String {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.expect("header");
            socket.write_all(&body).await.expect("body");
            let _ = socket.shutdown().await;
        });
        format!("http://{address}/model.onnx")
    }

    fn local_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client")
    }

    fn fake_model() -> (Vec<u8>, String) {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let digest = hex::encode(Sha256::digest(&body));
        (body, digest)
    }

    #[tokio::test]
    async fn download_verified_streams_hashes_and_renames() {
        let (body, digest) = fake_model();
        let url = serve_once(body.clone()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("model.onnx");
        let mut percents = Vec::new();
        download_verified(
            &local_client(),
            &url,
            &destination,
            body.len() as u64,
            &digest,
            |percent| percents.push(percent),
        )
        .await
        .expect("download should succeed");
        assert_eq!(std::fs::read(&destination).expect("read"), body);
        assert!(!destination.with_extension("part").exists());
        assert_eq!(percents.last(), Some(&100));
        assert!(percents.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    async fn download_verified_rejects_a_checksum_mismatch() {
        let (body, _) = fake_model();
        let url = serve_once(body.clone()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("model.onnx");
        let error = download_verified(
            &local_client(),
            &url,
            &destination,
            body.len() as u64,
            &"0".repeat(64),
            |_| {},
        )
        .await
        .expect_err("checksum mismatch must fail");
        assert!(error.to_string().contains("checksum"), "{error:#}");
        assert!(!destination.exists());
        assert!(!destination.with_extension("part").exists());
    }

    #[tokio::test]
    async fn download_verified_rejects_the_wrong_size() {
        let (body, digest) = fake_model();
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("model.onnx");
        for expected in [body.len() as u64 + 1, body.len() as u64 - 1] {
            let url = serve_once(body.clone()).await;
            let error = download_verified(
                &local_client(),
                &url,
                &destination,
                expected,
                &digest,
                |_| {},
            )
            .await
            .expect_err("size mismatch must fail");
            assert!(
                error.to_string().contains("incomplete") || error.to_string().contains("larger"),
                "{error:#}"
            );
            assert!(!destination.exists());
            assert!(!destination.with_extension("part").exists());
        }
    }

    /// Runs the Nemotron path of this module (runner, speaker cap, turn
    /// mapping, label stabilisation) on a real two-speaker clip:
    /// `MEETILY_SORTFORMER_MODEL=... MEETILY_DIARIZATION_FIXTURE=... cargo test
    /// -p meetily --lib audio::diarization -- --ignored`.
    #[test]
    #[ignore = "needs MEETILY_SORTFORMER_MODEL and MEETILY_DIARIZATION_FIXTURE"]
    fn nemotron_diarizes_two_speaker_fixture() {
        let model = std::env::var("MEETILY_SORTFORMER_MODEL")
            .expect("MEETILY_SORTFORMER_MODEL must point to the Nemotron-3 ONNX model");
        let fixture = std::env::var("MEETILY_DIARIZATION_FIXTURE")
            .expect("MEETILY_DIARIZATION_FIXTURE must point to a 16 kHz WAV file");
        let samples = decode_audio_file(Path::new(&fixture))
            .expect("fixture should decode")
            .to_whisper_format();
        let mut diarizer = SortformerDiarizer::new(Path::new(&model)).expect("model should load");
        let segments = diarizer
            .diarize(&samples, |_| {})
            .expect("fixture should diarize");

        let mut turns = segments_to_turns(cap_speakers(segments.clone(), 2));
        stabilize_turn_labels(&mut turns, &[]);
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

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires downloaded models and MEETILY_DIARIZATION_FIXTURE"]
    fn pyannote_diarizes_real_multi_speaker_fixture() {
        let fixture = std::env::var("MEETILY_DIARIZATION_FIXTURE")
            .expect("MEETILY_DIARIZATION_FIXTURE must point to a 16 kHz WAV file");
        let segmentation = std::env::var("MEETILY_SEGMENTATION_MODEL")
            .expect("MEETILY_SEGMENTATION_MODEL must point to model.int8.onnx");
        let embedding = std::env::var("MEETILY_EMBEDDING_MODEL")
            .expect("MEETILY_EMBEDDING_MODEL must point to an embedding ONNX model");
        let audio = decode_audio_file(Path::new(&fixture)).expect("fixture should decode");
        let turns = diarize_with_pyannote(
            &audio.to_whisper_format(),
            &PyannoteModels {
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
