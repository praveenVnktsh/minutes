//! The decoding engine behind live transcription, as a trait so the
//! segmentation in `transcriber` is independent of how the model runs. The
//! engine is `native::NativeRecognizer`: Core ML on the GPU or Neural Engine,
//! in the `speech-helper` sidecar.
//!
//! Streams are named by id: feed audio, decode what is ready, read the
//! hypothesis (tokens and their times), reset it, mark the input finished.

use anyhow::Result;

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
    /// Where it runs, for logs and provenance: "gpu" or "ane".
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
