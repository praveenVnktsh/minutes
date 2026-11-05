//! The batch speech jobs (import, retranscription, final pass, diarization,
//! mic check) through one process-wide `speech-helper batch` process.
//!
//! The helper starts on the first request and loads each model (Parakeet,
//! Silero VAD, pyannote) on first use, so it holds about 1.5 GB once warm;
//! [`unload`] stops it after a job. Requests are serialised: the helper takes
//! one at a time. VAD and diarization read a 16 kHz mono WAV the client
//! writes to a temporary file; transcription takes the samples in the frame.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use log::{info, warn};
use serde::Deserialize;
use serde_json::{json, Value};
use tempfile::NamedTempFile;

use super::helper::{self, op, Compute, Helper, Role, LOAD_TIMEOUT};

/// The helper's batch input rate.
pub const SAMPLE_RATE: u32 = 16_000;

/// A stretch of speech found by the VAD, cut from the input.
#[derive(Debug, Clone)]
pub struct SpeechSegment {
    pub samples: Vec<f32>,
    pub start_timestamp_ms: f64,
    pub end_timestamp_ms: f64,
}

/// One recognised word, in seconds from the start of the request's audio.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Transcript {
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub confidence: Option<f32>,
}

/// One speaker turn, in seconds. Speakers are numbered from 0 in order of
/// first appearance.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: u32,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Deserialize)]
struct Segments<T> {
    segments: Vec<T>,
    #[serde(default)]
    ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Span {
    start: f64,
    end: f64,
}

type Progress = Option<Box<dyn Fn(f32) + Send + Sync>>;
/// A request's payload and the temp file it points at (kept until the reply).
type Payload = (Vec<u8>, Option<NamedTempFile>);

/// Where the helper loads its models from; set by [`configure`].
static MODELS_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
static HELPER: Mutex<Option<Arc<Helper>>> = Mutex::new(None);
/// Held for a whole request: the helper answers one at a time.
static TURN: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Point the batch helper at the models directory. Called once at setup.
pub fn configure(models_dir: PathBuf) {
    info!("Batch speech models: {}", models_dir.display());
    *MODELS_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(models_dir);
}

/// Transcribe up to ~30 s of 16 kHz mono audio with Parakeet.
pub async fn transcribe(samples_16k: Vec<f32>) -> Result<Transcript> {
    if samples_16k.is_empty() {
        return Ok(Transcript::default());
    }
    let seconds = seconds_of(samples_16k.len());
    let payload = helper::audio_payload(&samples_16k);
    if payload.len() > helper::MAX_PAYLOAD {
        bail!("{seconds:.0} s of audio is too long for one transcription request");
    }
    let reply = request(
        op::TRANSCRIBE,
        move || Ok((payload, None)),
        timeout_for(seconds, 1.0),
        None,
    )
    .await?;
    serde_json::from_value(reply).context("unreadable speech-helper transcription reply")
}

/// The speech in 16 kHz mono audio (Silero VAD), each segment with its
/// samples. `on_progress` gets 0..1.
pub async fn speech_segments(
    samples_16k: &[f32],
    on_progress: Option<Box<dyn Fn(f32) + Send + Sync>>,
) -> Result<Vec<SpeechSegment>> {
    if samples_16k.is_empty() {
        return Ok(Vec::new());
    }
    let seconds = seconds_of(samples_16k.len());
    let wav = wav_bytes(samples_16k)?;
    let reply = request(
        op::VAD,
        wav_job(wav, json!({})),
        timeout_for(seconds, 0.25),
        on_progress,
    )
    .await?;
    let reply: Segments<Span> =
        serde_json::from_value(reply).context("unreadable speech-helper VAD reply")?;
    let segments = cut_segments(samples_16k, &reply.segments);
    info!(
        "VAD found {} speech segments in {seconds:.0} s of audio ({:.0} ms)",
        segments.len(),
        reply.ms.unwrap_or(0.0)
    );
    Ok(segments)
}

/// Who spoke when in 16 kHz mono audio (pyannote community-1 + WeSpeaker).
/// `max_speakers` is a hint the helper may ignore; callers cap the result.
/// `on_progress` gets 0..1.
pub async fn diarize(
    samples_16k: &[f32],
    max_speakers: Option<usize>,
    on_progress: Option<Box<dyn Fn(f32) + Send + Sync>>,
) -> Result<Vec<SpeakerSegment>> {
    if samples_16k.is_empty() {
        return Ok(Vec::new());
    }
    let seconds = seconds_of(samples_16k.len());
    let wav = wav_bytes(samples_16k)?;
    let reply = request(
        op::DIARIZE,
        wav_job(wav, json!({ "max_speakers": max_speakers.unwrap_or(0) })),
        timeout_for(seconds, 0.5),
        on_progress,
    )
    .await?;
    let reply: Segments<SpeakerSegment> =
        serde_json::from_value(reply).context("unreadable speech-helper diarization reply")?;
    let mut segments: Vec<SpeakerSegment> = reply
        .segments
        .into_iter()
        .filter(|s| s.start.is_finite() && s.end.is_finite() && s.end > s.start)
        .collect();
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    info!(
        "Diarization found {} turns in {seconds:.0} s of audio ({:.0} ms)",
        segments.len(),
        reply.ms.unwrap_or(0.0)
    );
    Ok(segments)
}

/// Whether the batch helper process is running.
pub fn is_loaded() -> bool {
    lock_helper()
        .as_ref()
        .is_some_and(|helper| !helper.is_broken())
}

/// Stop the batch helper (frees its models), after any request in flight.
pub async fn unload() {
    let _turn = TURN.lock().await;
    let helper = lock_helper().take();
    if let Some(helper) = helper {
        info!("Stopping the batch speech-helper");
        // Drop waits for the process to exit.
        let _ = tokio::task::spawn_blocking(move || drop(helper)).await;
    }
}

/// [`unload`], unless a request is in flight: then nothing happens and it
/// returns false. For deleting a model, which must not wait out a long job.
pub async fn try_unload() -> bool {
    let Ok(_turn) = TURN.try_lock() else {
        return false;
    };
    let helper = lock_helper().take();
    if let Some(helper) = helper {
        info!("Stopping the batch speech-helper");
        let _ = tokio::task::spawn_blocking(move || drop(helper)).await;
    }
    true
}

fn lock_helper() -> std::sync::MutexGuard<'static, Option<Arc<Helper>>> {
    HELPER.lock().unwrap_or_else(|e| e.into_inner())
}

fn seconds_of(samples: usize) -> f64 {
    samples as f64 / f64::from(SAMPLE_RATE)
}

/// The first request of a kind loads its model (up to [`LOAD_TIMEOUT`]),
/// then the work scales with the audio.
fn timeout_for(seconds: f64, per_second: f64) -> Duration {
    LOAD_TIMEOUT + Duration::from_secs_f64((seconds * per_second).max(0.0))
}

/// The running helper, or a new one in place of a missing or broken one.
/// Blocking (a replaced helper is dropped here).
fn current_helper() -> Result<Arc<Helper>> {
    let mut slot = lock_helper();
    if let Some(helper) = slot.as_ref().filter(|helper| !helper.is_broken()) {
        return Ok(helper.clone());
    }
    let dir = MODELS_DIR
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| anyhow!("the speech models are not set up"))?;
    let helper = Arc::new(Helper::spawn(Role::Batch, &dir, Compute::All)?);
    *slot = Some(helper.clone());
    Ok(helper)
}

/// Run one request on the batch helper, waiting for its turn. `payload`
/// builds the frame's payload (and the temp file it points at, kept until the
/// reply) on the blocking thread.
async fn request(
    op: u8,
    payload: impl FnOnce() -> Result<Payload> + Send + 'static,
    timeout: Duration,
    on_progress: Progress,
) -> Result<Value> {
    let _turn = TURN.lock().await;
    tokio::task::spawn_blocking(move || {
        let (payload, _file) = payload()?;
        let helper = current_helper()?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut report = |progress: f32| {
            if let Some(on_progress) = &on_progress {
                on_progress(progress);
            }
        };
        let result = helper.exchange_with_progress(op, id, &payload, timeout, &mut report);
        match &result {
            Ok(_) => report(1.0),
            Err(error) if helper.is_broken() => {
                warn!("Batch speech-helper failed; it restarts on the next request: {error:#}");
                let mut slot = lock_helper();
                if slot
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &helper))
                {
                    slot.take();
                }
            }
            Err(_) => {}
        }
        result
    })
    .await
    .context("speech-helper request task failed")?
}

/// A VAD/diarize payload: the audio as a temp WAV, its path merged into
/// `fields`.
fn wav_job(wav: Vec<u8>, mut fields: Value) -> impl FnOnce() -> Result<Payload> + Send + 'static {
    move || {
        let mut file = tempfile::Builder::new()
            .prefix("minutes-speech-")
            .suffix(".wav")
            .tempfile()
            .context("could not create a temporary WAV for the speech-helper")?;
        file.write_all(&wav)
            .and_then(|_| file.flush())
            .context("could not write a temporary WAV for the speech-helper")?;
        fields["path"] = Value::String(file.path().to_string_lossy().into_owned());
        Ok((serde_json::to_vec(&fields)?, Some(file)))
    }
}

/// 16 kHz mono 16-bit PCM WAV.
fn wav_bytes(samples: &[f32]) -> Result<Vec<u8>> {
    let data_len = samples
        .len()
        .checked_mul(2)
        .filter(|len| *len <= (u32::MAX - 36) as usize)
        .ok_or_else(|| anyhow!("audio is too long for a WAV file"))?;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(out)
}

/// Cut each VAD span (seconds) out of `samples`, clamped to the audio, in
/// time order; empty spans are dropped.
fn cut_segments(samples: &[f32], spans: &[Span]) -> Vec<SpeechSegment> {
    let rate = f64::from(SAMPLE_RATE);
    let index = |seconds: f64| ((seconds * rate).round().max(0.0) as usize).min(samples.len());
    let mut segments: Vec<SpeechSegment> = spans
        .iter()
        .filter_map(|span| {
            let (start, end) = (index(span.start), index(span.end));
            (end > start).then(|| SpeechSegment {
                samples: samples[start..end].to_vec(),
                start_timestamp_ms: start as f64 * 1000.0 / rate,
                end_timestamp_ms: end as f64 * 1000.0 / rate,
            })
        })
        .collect();
    segments.sort_by(|a, b| a.start_timestamp_ms.total_cmp(&b.start_timestamp_ms));
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_replies() {
        let transcript: Transcript = serde_json::from_str(
            r#"{"id":3,"text":"hello world","words":[{"text":"hello","start":0.12,"end":0.4}],"confidence":0.93,"ms":120}"#,
        )
        .unwrap();
        assert_eq!(transcript.text, "hello world");
        assert_eq!(
            transcript.words,
            vec![Word {
                text: "hello".into(),
                start: 0.12,
                end: 0.4
            }]
        );
        assert_eq!(transcript.confidence, Some(0.93_f64 as f32));
        let bare: Transcript =
            serde_json::from_str(r#"{"id":4,"text":"","words":[],"confidence":null,"ms":5}"#)
                .unwrap();
        assert_eq!(bare, Transcript::default());
    }

    #[test]
    fn segment_replies() {
        let vad: Segments<Span> =
            serde_json::from_str(r#"{"id":1,"segments":[{"start":0.52,"end":4.1}],"ms":40}"#)
                .unwrap();
        assert_eq!(vad.segments.len(), 1);
        assert_eq!(vad.segments[0].end, 4.1);
        assert_eq!(vad.ms, Some(40.0));
        let speakers: Segments<SpeakerSegment> = serde_json::from_str(
            r#"{"id":2,"segments":[{"speaker":0,"start":1.2,"end":3.4},{"speaker":1,"start":3.4,"end":5}]}"#,
        )
        .unwrap();
        assert_eq!(
            speakers.segments[1],
            SpeakerSegment {
                speaker: 1,
                start: 3.4,
                end: 5.0
            }
        );
    }

    #[test]
    fn wav_header_describes_16k_mono_pcm() {
        let wav = wav_bytes(&[0.0, 1.0, -1.0, 2.0]).unwrap();
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
        let samples: Vec<i16> = wav[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(samples, vec![0, i16::MAX, -i16::MAX, i16::MAX]);
    }

    #[test]
    fn segments_are_cut_from_the_input_and_clamped() {
        let samples: Vec<f32> = (0..32_000).map(|i| i as f32).collect();
        let spans = [
            Span {
                start: 1.5,
                end: 3.0,
            },
            Span {
                start: 0.5,
                end: 1.0,
            },
            Span {
                start: 1.9,
                end: 1.9,
            },
            Span {
                start: -0.1,
                end: 0.0001,
            },
        ];
        let segments = cut_segments(&samples, &spans);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].start_timestamp_ms, 0.0);
        assert_eq!(segments[0].samples.len(), 2);
        assert_eq!(segments[1].start_timestamp_ms, 500.0);
        assert_eq!(segments[1].end_timestamp_ms, 1000.0);
        assert_eq!(segments[1].samples.first(), Some(&8_000.0));
        assert_eq!(segments[1].samples.len(), 8_000);
        // Clamped to the 2 s of input.
        assert_eq!(segments[2].start_timestamp_ms, 1500.0);
        assert_eq!(segments[2].end_timestamp_ms, 2000.0);
        assert_eq!(segments[2].samples.last(), Some(&31_999.0));
    }

    #[test]
    fn wav_jobs_point_at_a_temp_file() {
        let job = wav_job(
            wav_bytes(&[0.0; 160]).unwrap(),
            json!({ "max_speakers": 3 }),
        );
        let (payload, file) = job().unwrap();
        let body: Value = serde_json::from_slice(&payload).unwrap();
        let file = file.unwrap();
        assert_eq!(body["max_speakers"], 3);
        assert_eq!(body["path"].as_str(), file.path().to_str());
        assert_eq!(std::fs::metadata(file.path()).unwrap().len(), 44 + 320);
    }

    /// Against the real helper and models: set `MINUTES_SPEECH_HELPER` (a
    /// built `speech-helper`) and `MINUTES_SPEECH_MODELS` (the FluidAudio
    /// models directory), then run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn the_batch_helper_answers_each_request() {
        let (Ok(_), Ok(models)) = (
            std::env::var("MINUTES_SPEECH_HELPER"),
            std::env::var("MINUTES_SPEECH_MODELS"),
        ) else {
            return;
        };
        configure(PathBuf::from(models));
        // A 440 Hz tone then silence: no words or speakers expected, but every
        // request must answer.
        let mut samples: Vec<f32> = (0..32_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 16_000.0).sin() * 0.1)
            .collect();
        samples.resize(64_000, 0.0);
        transcribe(samples.clone()).await.unwrap();
        assert!(is_loaded());
        speech_segments(&samples, None).await.unwrap();
        let progress = Arc::new(Mutex::new(Vec::new()));
        let seen = progress.clone();
        diarize(
            &samples,
            Some(2),
            Some(Box::new(move |p: f32| seen.lock().unwrap().push(p))),
        )
        .await
        .unwrap();
        assert_eq!(progress.lock().unwrap().last(), Some(&1.0));
        unload().await;
        assert!(!is_loaded());
    }
}
