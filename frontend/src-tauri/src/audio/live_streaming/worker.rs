//! The live transcription worker for the streaming model.
//!
//! Receives continuous per-source windows from the pipeline (streaming mode),
//! runs them through `StreamingTranscriber` on a blocking thread, and emits:
//! - `transcript-partial` `{source, text, audio_start_time}`: the utterance in
//!   progress, display only, never saved (empty text clears it);
//! - `transcript-update`: finished utterances, through the same dedup and
//!   sequence ids as the chunk worker, so saving and the UI are unchanged.
//!
//! An empty-data chunk is a flush marker (pause or stop) for its source. The
//! task ends when the pipeline drops its sender, after flushing both sources,
//! which is what the stop path waits for, or as soon as it sees
//! [`cancel_current`], which the stop path uses when draining takes too long.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tokio::sync::mpsc::UnboundedReceiver;

use super::engine::StreamingEngine;
use crate::audio::{AudioChunk, RecordingDeviceType};

pub const PARTIAL_EVENT: &str = "transcript-partial";

/// Queued audio beyond which live text is too stale to be useful: the backlog
/// is dropped from the live view (the final pass still covers it). This also
/// bounds what a stop has to decode before it can save.
pub const MAX_BACKLOG_SECONDS: f64 = 20.0;

/// The live worker's latest backlog, in milliseconds of queued audio. Other
/// features (Copilot's proactive suggestions) read it to back off when live
/// transcription is falling behind real time.
static LAST_BACKLOG_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Seconds of audio live transcription has queued: the streaming worker's
/// backlog at its last decode, or the chunked (Whisper/Parakeet) worker's
/// queue depth, whichever is larger. 0 when neither runs.
pub fn current_backlog_seconds() -> f64 {
    let streaming = LAST_BACKLOG_MS.load(std::sync::atomic::Ordering::Relaxed);
    streaming.max(chunked_backlog_ms()) as f64 / 1000.0
}

/// Audio (ms) queued in the chunked live worker, per source (microphone,
/// system). The sources run side by side, so the backlog is the larger one.
static CHUNKED_QUEUED_MS: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Decode time and recording time the streaming worker has processed, in
/// microseconds, since the process started: their ratio over an interval is
/// live transcription's real-time factor. Read by the replay harness.
static DECODE_BUSY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DECODED_AUDIO_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `(busy seconds, recording seconds)` the streaming worker has decoded so far.
pub fn decode_totals() -> (f64, f64) {
    (
        DECODE_BUSY_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        DECODED_AUDIO_US.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
    )
}

fn chunked_backlog_ms() -> u64 {
    CHUNKED_QUEUED_MS
        .iter()
        .map(|queued| queued.load(std::sync::atomic::Ordering::Relaxed))
        .max()
        .unwrap_or(0)
}

/// Counts one chunk as queued in the chunked live worker until dropped (when
/// the worker is done with it, whatever the outcome).
pub struct ChunkedBacklog {
    source: usize,
    ms: u64,
}

impl ChunkedBacklog {
    pub fn queued(system: bool, samples: usize, sample_rate: u32) -> Self {
        let ms = (samples as u64 * 1000) / u64::from(sample_rate.max(1));
        let source = usize::from(system);
        CHUNKED_QUEUED_MS[source].fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
        Self { source, ms }
    }
}

impl Drop for ChunkedBacklog {
    fn drop(&mut self) {
        let queued = &CHUNKED_QUEUED_MS[self.source];
        let _ = queued.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |current| Some(current.saturating_sub(self.ms)),
        );
    }
}

/// Cancel flag of the running streaming worker, if any.
static CURRENT_CANCEL: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Bumped on every cancel, so an engine blocked waiting for its models or a
/// decode (see `native.rs`) can notice a cancel while it waits.
pub static CANCEL_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Ask the running streaming worker to stop now, without decoding what is
/// still queued. It notices within `RECV_POLL` (or after the decode step in
/// progress), drops its recognizer handle and ends. Returns whether a worker
/// was running.
pub fn cancel_current() -> bool {
    CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst);
    match CURRENT_CANCEL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

fn register_cancel() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    *CURRENT_CANCEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag.clone());
    flag
}

fn clear_cancel(flag: &Arc<AtomicBool>) {
    let mut current = CURRENT_CANCEL.lock().unwrap_or_else(|e| e.into_inner());
    if current.as_ref().is_some_and(|f| Arc::ptr_eq(f, flag)) {
        *current = None;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptPartial {
    pub source: &'static str,
    pub text: String,
    pub audio_start_time: f64,
}

fn source_index(device: &RecordingDeviceType) -> usize {
    match device {
        RecordingDeviceType::Microphone => 0,
        RecordingDeviceType::System => 1,
    }
}

/// Seconds of recording time waiting in `queued`. Each source carries its own
/// timeline and the sources run side by side, so the backlog is the longest
/// per-source queue, not the sum: with one source it is that source's audio,
/// with two it is not double-counted.
pub fn backlog_seconds<'a>(queued: impl IntoIterator<Item = &'a AudioChunk>) -> f64 {
    let mut per_source = [0.0f64; 2];
    for chunk in queued {
        per_source[source_index(&chunk.device_type)] +=
            chunk.data.len() as f64 / chunk.sample_rate.max(1) as f64;
    }
    per_source[0].max(per_source[1])
}

/// Drop the stale audio of an over-long backlog, keeping flush markers (pause
/// or stop boundaries) in order. Returns how many windows were dropped.
pub fn drop_stale_windows(queued: &mut Vec<AudioChunk>) -> usize {
    let before = queued.len();
    queued.retain(|chunk| chunk.data.is_empty());
    before - queued.len()
}

mod imp {
    use super::*;
    use std::time::Duration;

    use log::{info, warn};
    use tauri::Emitter;

    use crate::audio::live_streaming::transcriber::{LiveEvent, Source, StreamingTranscriber};
    use crate::audio::transcription::TranscriptDeduper;

    /// Streaming results carry no per-segment confidence.
    const STREAMING_CONFIDENCE: f32 = 0.9;
    /// How often a worker waiting for audio checks its cancel flag.
    const RECV_POLL: Duration = Duration::from_millis(250);

    fn emit_events<R: Runtime>(
        app: &AppHandle<R>,
        deduper: &mut TranscriptDeduper,
        offset: f64,
        events: &mut Vec<LiveEvent>,
    ) {
        for event in events.drain(..) {
            match event {
                LiveEvent::Partial {
                    source,
                    text,
                    start,
                } => {
                    let _ = app.emit(
                        PARTIAL_EVENT,
                        TranscriptPartial {
                            source: source.label(),
                            text,
                            audio_start_time: start + offset,
                        },
                    );
                }
                LiveEvent::Final {
                    source,
                    text,
                    start,
                    end,
                } => {
                    perf_debug!(
                        "Streaming final [{}] {:.2}-{:.2}: {}",
                        source.label(),
                        start,
                        end,
                        text
                    );
                    crate::audio::transcription::worker::emit_final_segment(
                        app,
                        deduper,
                        text,
                        source.label(),
                        start + offset,
                        end + offset,
                        STREAMING_CONFIDENCE,
                    );
                    let _ = app.emit(
                        PARTIAL_EVENT,
                        TranscriptPartial {
                            source: source.label(),
                            text: String::new(),
                            audio_start_time: start + offset,
                        },
                    );
                }
            }
        }
    }

    enum Next {
        Chunk(AudioChunk),
        Closed,
        Cancelled,
    }

    fn next_chunk(
        runtime: &tokio::runtime::Handle,
        receiver: &mut UnboundedReceiver<AudioChunk>,
        cancel: &AtomicBool,
    ) -> Next {
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Next::Cancelled;
            }
            match runtime.block_on(tokio::time::timeout(RECV_POLL, receiver.recv())) {
                Ok(Some(chunk)) => return Next::Chunk(chunk),
                Ok(None) => return Next::Closed,
                Err(_) => continue,
            }
        }
    }

    pub fn start<R: Runtime>(
        app: AppHandle<R>,
        mut receiver: UnboundedReceiver<AudioChunk>,
        engine: Arc<StreamingEngine>,
    ) -> tokio::task::JoinHandle<()> {
        let cancel = register_cancel();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let offset = crate::audio::recording_state::audio_time_offset_seconds();
            let mut deduper = TranscriptDeduper::new();
            let mut transcriber: Option<StreamingTranscriber> = None;
            let mut events = Vec::new();
            let mut warned_backlog = false;
            let mut cancelled = false;
            let mut engine = engine;
            let mut live_text_off = false;
            info!(
                "🚀 Streaming live transcription started (Nemotron on {})",
                engine.device()
            );

            loop {
                let first = match next_chunk(&runtime, &mut receiver, &cancel) {
                    Next::Chunk(chunk) => chunk,
                    Next::Closed => break,
                    Next::Cancelled => {
                        cancelled = true;
                        break;
                    }
                };
                // Take everything already queued, so both sources' windows are
                // fed before one batched decode, and so the backlog is measured
                // on what is really waiting.
                let mut batch = vec![first];
                while let Ok(chunk) = receiver.try_recv() {
                    batch.push(chunk);
                }
                if live_text_off {
                    continue;
                }

                let backlog = backlog_seconds(&batch);
                let busy_since = std::time::Instant::now();
                super::LAST_BACKLOG_MS.store(
                    (backlog.max(0.0) * 1000.0) as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                if backlog > MAX_BACKLOG_SECONDS {
                    // Windows carry their recording position, so the next one
                    // realigns each source's timeline.
                    let dropped = drop_stale_windows(&mut batch);
                    let transcriber = transcriber.get_or_insert_with(|| {
                        StreamingTranscriber::new(engine.clone(), first_rate(&batch))
                    });
                    transcriber.flush_source(Source::Microphone, &mut events);
                    transcriber.flush_source(Source::System, &mut events);
                    perf_debug!("Dropped {} queued live windows", dropped);
                    warn!(
                        "Live transcription fell {:.0}s behind; skipped ahead (the final transcript covers it)",
                        backlog
                    );
                    if !warned_backlog {
                        warned_backlog = true;
                        let _ = app.emit(
                            "transcription-error",
                            serde_json::json!({
                                "error": "live transcription backlog",
                                "userMessage": "Live text fell behind and skipped ahead. The final transcript after the meeting will include everything.",
                                "actionable": false,
                                "phase": "active"
                            }),
                        );
                    }
                }

                for chunk in batch {
                    let source = Source::from_device(&chunk.device_type);
                    let transcriber = transcriber.get_or_insert_with(|| {
                        StreamingTranscriber::new(engine.clone(), chunk.sample_rate)
                    });
                    if chunk.data.is_empty() {
                        transcriber.flush_source(source, &mut events);
                        continue;
                    }
                    transcriber.push(source, &chunk.data, chunk.timestamp, &mut events);
                }

                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                if let Some(current) = transcriber.as_mut() {
                    current.decode(&mut events);
                    if let Some(failure) = current.failure().map(str::to_string) {
                        // The Core ML helper died or hung. Keep what it
                        // decoded and carry on on the CPU if that model is
                        // there; the final pass covers any gap either way.
                        current.commit_decoded(&mut events);
                        let sample_rate = current.sample_rate();
                        transcriber = None;
                        engine = match fallback_engine(&app, &engine) {
                            Some(cpu) => {
                                notify_engine_failure(&app, &failure, true);
                                cpu
                            }
                            None => {
                                notify_engine_failure(&app, &failure, false);
                                engine
                            }
                        };
                        if engine.backend == crate::audio::live_streaming::engine::Backend::Sherpa {
                            transcriber =
                                Some(StreamingTranscriber::new(engine.clone(), sample_rate));
                        } else {
                            // No fallback: drain without live text.
                            live_text_off = true;
                        }
                    }
                }
                emit_events(&app, &mut deduper, offset, &mut events);
                super::DECODE_BUSY_US.fetch_add(
                    busy_since.elapsed().as_micros() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                super::DECODED_AUDIO_US.fetch_add(
                    (backlog.max(0.0) * 1e6) as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }

            if cancelled {
                warn!("Streaming live transcription cancelled; keeping the text decoded so far");
                // Without the automatic final pass the live transcript is the
                // one kept: save the utterance in progress, don't drop it.
                if let Some(transcriber) = transcriber.as_mut() {
                    transcriber.commit_decoded(&mut events);
                }
            } else if let Some(transcriber) = transcriber.as_mut() {
                transcriber.finish(&mut events);
            }
            emit_events(&app, &mut deduper, offset, &mut events);
            // Drop the recognizer (and its streams) before reporting done, so
            // the stop path's unload really frees the model.
            drop(transcriber);
            drop(engine);
            clear_cancel(&cancel);
            super::LAST_BACKLOG_MS.store(0, std::sync::atomic::Ordering::Relaxed);
            info!("✅ Streaming live transcription finished");
        })
    }

    /// The CPU engine to continue on after the Core ML helper failed, if the
    /// failed engine was not already the CPU one and its model is on disk.
    fn fallback_engine<R: Runtime>(
        app: &AppHandle<R>,
        failed: &Arc<StreamingEngine>,
    ) -> Option<Arc<StreamingEngine>> {
        if failed.backend == crate::audio::live_streaming::engine::Backend::Sherpa {
            return None;
        }
        match crate::audio::live_streaming::engine::load_cpu_fallback(app) {
            Ok(engine) => Some(engine),
            Err(error) => {
                warn!("No CPU fallback for live transcription: {error:#}");
                None
            }
        }
    }

    fn notify_engine_failure<R: Runtime>(app: &AppHandle<R>, failure: &str, recovered: bool) {
        let message = if recovered {
            "Live transcription on the GPU/Neural Engine stopped, so it continues on the CPU."
        } else {
            "Live transcription stopped. The final transcript after the meeting will include everything."
        };
        warn!("{message} ({failure})");
        let _ = app.emit(
            "transcription-error",
            serde_json::json!({
                "error": failure,
                "userMessage": message,
                "actionable": false,
                "phase": "active"
            }),
        );
    }

    fn first_rate(batch: &[AudioChunk]) -> u32 {
        batch.first().map(|c| c.sample_rate).unwrap_or(48_000)
    }
}

/// Start the streaming worker. Off macOS there is no recognizer to run (the
/// engine never loads, see `live_streaming`), so this only drains the
/// receiver until the pipeline closes it.
pub fn start_streaming_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    receiver: UnboundedReceiver<AudioChunk>,
    engine: Arc<StreamingEngine>,
) -> tokio::task::JoinHandle<()> {
    imp::start(app, receiver, engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(device: RecordingDeviceType, seconds: f64) -> AudioChunk {
        AudioChunk {
            data: vec![0.1; (seconds * 48_000.0) as usize],
            sample_rate: 48_000,
            timestamp: 0.0,
            chunk_id: 0,
            device_type: device,
        }
    }

    fn marker(device: RecordingDeviceType) -> AudioChunk {
        AudioChunk {
            data: Vec::new(),
            sample_rate: 48_000,
            timestamp: 0.0,
            chunk_id: 0,
            device_type: device,
        }
    }

    #[test]
    fn the_chunked_queue_counts_toward_the_backlog_until_each_chunk_is_done() {
        let before = current_backlog_seconds();
        let system = ChunkedBacklog::queued(true, 48_000 * 6, 48_000);
        let mic = ChunkedBacklog::queued(false, 48_000 * 2, 48_000);
        assert!(current_backlog_seconds() >= before.max(6.0) - 1e-9);
        drop(system);
        assert!(current_backlog_seconds() < 6.0);
        drop(mic);
        assert!(chunked_backlog_ms() == 0 || current_backlog_seconds() < 2.0);
    }

    #[test]
    fn backlog_is_the_longest_source_queue() {
        // Two sources side by side: 10 s each is 10 s behind, not 20.
        let both: Vec<_> = (0..200)
            .flat_map(|_| {
                [
                    window(RecordingDeviceType::Microphone, 0.05),
                    window(RecordingDeviceType::System, 0.05),
                ]
            })
            .collect();
        assert!((backlog_seconds(&both) - 10.0).abs() < 1e-6);
        // One source only (the other is silent or absent): its whole queue.
        let mic_only: Vec<_> = (0..200)
            .map(|_| window(RecordingDeviceType::Microphone, 0.05))
            .collect();
        assert!((backlog_seconds(&mic_only) - 10.0).abs() < 1e-6);
        assert_eq!(backlog_seconds(&[]), 0.0);
    }

    #[test]
    fn a_long_drained_batch_counts_as_backlog() {
        // Regression: the backlog used to be measured on the channel after it
        // had been drained into the batch, so it was always ~0.
        let batch: Vec<_> = (0..500)
            .map(|_| window(RecordingDeviceType::System, 0.05))
            .collect();
        assert!(backlog_seconds(&batch) > MAX_BACKLOG_SECONDS);
    }

    #[test]
    fn skipping_ahead_keeps_flush_markers_in_order() {
        let mut batch = vec![
            window(RecordingDeviceType::Microphone, 1.0),
            marker(RecordingDeviceType::Microphone),
            window(RecordingDeviceType::System, 1.0),
            marker(RecordingDeviceType::System),
        ];
        assert_eq!(drop_stale_windows(&mut batch), 2);
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(|c| c.data.is_empty()));
        assert!(matches!(
            batch[0].device_type,
            RecordingDeviceType::Microphone
        ));
    }

    #[test]
    fn cancel_reaches_the_registered_worker_only_once() {
        let flag = register_cancel();
        assert!(cancel_current());
        assert!(flag.load(Ordering::SeqCst));
        assert!(!cancel_current());
        clear_cancel(&flag);
    }
}
