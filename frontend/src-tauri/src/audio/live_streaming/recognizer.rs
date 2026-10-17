//! The decoding engine behind live transcription, as a trait so the
//! segmentation in `transcriber` runs unchanged on either engine:
//! - [`SherpaRecognizer`]: sherpa-onnx on the CPU (the 560 ms int8 export);
//! - `native::NativeRecognizer`: Core ML on the GPU or Neural Engine, in the
//!   `nemotron-helper` sidecar.
//!
//! The API is sherpa's `OnlineStream` one, with streams named by id: feed
//! audio, decode what is ready, read the hypothesis (tokens and their times),
//! reset it, mark the input finished.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use sherpa_onnx::{OnlineRecognizer, OnlineStream};

pub type StreamId = u32;

/// A stream's current hypothesis. `times[i]` is when `tokens[i]` was emitted,
/// in seconds after `start`, which is itself seconds after the stream began
/// (it moves on `reset`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hypothesis {
    pub tokens: Vec<String>,
    pub times: Vec<f64>,
    pub start: f64,
}

pub trait Recognizer: Send + Sync {
    /// Where it runs, for logs and provenance: "cpu", "gpu" or "ane".
    fn device(&self) -> &'static str;
    /// Block until the models are loaded (an engine may load in the
    /// background after it is created). Decoding waits for it anyway.
    fn wait_ready(&self) -> Result<()> {
        Ok(())
    }
    fn create_stream(&self, sample_rate: u32) -> Result<StreamId>;
    fn accept(&self, id: StreamId, sample_rate: u32, samples: &[f32]);
    /// No more audio for this stream; the next decode finishes it.
    fn input_finished(&self, id: StreamId);
    /// Decode everything that is ready on these streams (batched where the
    /// engine can).
    fn decode(&self, ids: &[StreamId]) -> Result<()>;
    fn result(&self, id: StreamId) -> Option<Hypothesis>;
    /// Clear the hypothesis; the stream carries on.
    fn reset(&self, id: StreamId);
    fn close(&self, id: StreamId);
}

/// sherpa-onnx's online recognizer (CPU).
pub struct SherpaRecognizer {
    recognizer: OnlineRecognizer,
    streams: Mutex<HashMap<StreamId, OnlineStream>>,
    next_id: AtomicU32,
}

impl SherpaRecognizer {
    pub fn new(recognizer: OnlineRecognizer) -> Self {
        Self {
            recognizer,
            streams: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
        }
    }

    fn with_stream<T>(&self, id: StreamId, f: impl FnOnce(&OnlineStream) -> T) -> Option<T> {
        let streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams.get(&id).map(f)
    }
}

impl Recognizer for SherpaRecognizer {
    fn device(&self) -> &'static str {
        "cpu"
    }

    fn create_stream(&self, _sample_rate: u32) -> Result<StreamId> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let stream = self.recognizer.create_stream();
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, stream);
        Ok(id)
    }

    fn accept(&self, id: StreamId, sample_rate: u32, samples: &[f32]) {
        self.with_stream(id, |s| s.accept_waveform(sample_rate as i32, samples));
    }

    fn input_finished(&self, id: StreamId) {
        self.with_stream(id, |s| s.input_finished());
    }

    fn decode(&self, ids: &[StreamId]) -> Result<()> {
        let streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        let selected: Vec<&OnlineStream> = ids.iter().filter_map(|id| streams.get(id)).collect();
        if selected.len() != ids.len() {
            return Err(anyhow!("decode of a closed live stream"));
        }
        loop {
            let ready: Vec<&OnlineStream> = selected
                .iter()
                .copied()
                .filter(|s| self.recognizer.is_ready(s))
                .collect();
            match ready.len() {
                0 => return Ok(()),
                1 => self.recognizer.decode(ready[0]),
                _ => self.recognizer.decode_multiple_streams(&ready),
            }
        }
    }

    fn result(&self, id: StreamId) -> Option<Hypothesis> {
        self.with_stream(id, |s| self.recognizer.get_result(s))
            .flatten()
            .map(|result| Hypothesis {
                times: result
                    .timestamps
                    .unwrap_or_default()
                    .iter()
                    .map(|&t| t as f64)
                    .collect(),
                tokens: result.tokens,
                start: result.start_time.unwrap_or(0.0) as f64,
            })
    }

    fn reset(&self, id: StreamId) {
        self.with_stream(id, |s| self.recognizer.reset(s));
    }

    fn close(&self, id: StreamId) {
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}
