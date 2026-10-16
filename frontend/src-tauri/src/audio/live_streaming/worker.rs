//! The live transcription worker for the streaming model.
//!
//! Receives continuous per-source windows from the pipeline (streaming mode),
//! runs them through [`StreamingTranscriber`] on a blocking thread, and emits:
//! - `transcript-partial` `{source, text, audio_start_time}`: the utterance in
//!   progress, display only, never saved (empty text clears it);
//! - `transcript-update`: finished utterances, through the same dedup and
//!   sequence ids as the chunk worker, so saving and the UI are unchanged.
//!
//! An empty-data chunk is a flush marker (pause or stop) for its source. The
//! task ends when the pipeline drops its sender, after flushing both sources,
//! which is what the stop path waits for.

use std::sync::Arc;

use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::mpsc::UnboundedReceiver;

use super::engine::StreamingEngine;
use super::transcriber::{LiveEvent, Source, StreamingTranscriber};
use crate::audio::transcription::TranscriptDeduper;
use crate::audio::AudioChunk;

pub const PARTIAL_EVENT: &str = "transcript-partial";

/// Queued audio beyond which live text is too stale to be useful: the backlog
/// is dropped from the live view (the final pass still covers it).
const MAX_BACKLOG_SECONDS: f64 = 20.0;
/// Streaming results carry no per-segment confidence.
const STREAMING_CONFIDENCE: f32 = 0.9;

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptPartial {
    pub source: &'static str,
    pub text: String,
    pub audio_start_time: f64,
}

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

/// Seconds of audio waiting in the channel, estimated from the latest window.
fn backlog_seconds(queued_chunks: usize, window_samples: usize, sample_rate: u32) -> f64 {
    // Windows alternate between the two sources, so two make one window of time.
    queued_chunks as f64 * window_samples as f64 / sample_rate.max(1) as f64 / 2.0
}

pub fn start_streaming_transcription_task<R: Runtime>(
    app: AppHandle<R>,
    mut receiver: UnboundedReceiver<AudioChunk>,
    engine: Arc<StreamingEngine>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let offset = crate::audio::recording_state::audio_time_offset_seconds();
        let mut deduper = TranscriptDeduper::new();
        let mut transcriber: Option<StreamingTranscriber> = None;
        let mut events = Vec::new();
        let mut warned_backlog = false;
        info!("🚀 Streaming live transcription started (Nemotron)");

        while let Some(first) = receiver.blocking_recv() {
            // Take everything already queued, so both sources' windows are fed
            // before one batched decode.
            let mut batch = vec![first];
            while let Ok(chunk) = receiver.try_recv() {
                batch.push(chunk);
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

                let backlog = backlog_seconds(receiver.len(), chunk.data.len(), chunk.sample_rate);
                if backlog > MAX_BACKLOG_SECONDS {
                    // Drop the queue from the live view. Windows carry their
                    // recording position, so the next one realigns the timeline.
                    let mut dropped = 0usize;
                    while let Ok(stale) = receiver.try_recv() {
                        if stale.data.is_empty() {
                            // Keep pause/stop boundaries.
                            transcriber
                                .flush_source(Source::from_device(&stale.device_type), &mut events);
                        }
                        dropped += 1;
                    }
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
            }

            if let Some(transcriber) = transcriber.as_mut() {
                transcriber.decode(&mut events);
            }
            emit_events(&app, &mut deduper, offset, &mut events);
        }

        if let Some(transcriber) = transcriber.as_mut() {
            transcriber.finish(&mut events);
        }
        emit_events(&app, &mut deduper, offset, &mut events);
        info!("✅ Streaming live transcription finished");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backlog_counts_both_sources_as_one_timeline() {
        // 400 queued 50 ms windows at 48 kHz = 200 windows per source = 10 s.
        assert!((backlog_seconds(400, 2400, 48_000) - 10.0).abs() < 1e-9);
        assert_eq!(backlog_seconds(0, 2400, 48_000), 0.0);
    }
}
