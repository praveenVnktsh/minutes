//! Nemotron Speech Streaming on Core ML (GPU or Neural Engine), run by
//! FluidAudio in the `speech-helper` sidecar's live role (Swift,
//! `speech-helper/` in the repo).
//!
//! The process, framing, readiness and failure latch are [`Helper`]'s
//! (`audio/speech/helper.rs`). Feeding audio and stream control are
//! reply-less frames, so they cost a pipe write; only `decode` has a reply,
//! carrying the hypothesis of every open stream. The protocol is documented
//! in `speech-helper/Sources/SpeechCore/Protocol.swift`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::recognizer::{Hypothesis, Recognizer, StreamId};
use crate::audio::speech::helper::{audio_payload, op, Compute, Helper, Role};

/// How long a decode may take before the helper is considered stuck. A
/// decode covers at most the queued backlog (bounded by the worker).
const DECODE_TIMEOUT: Duration = Duration::from_secs(30);
/// First load of a model compiles it for the device; later loads hit the
/// system's cache.
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
pub struct StreamReply {
    pub id: StreamId,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub tokens: Vec<String>,
    #[serde(default)]
    pub times: Vec<f64>,
}

/// `decode`'s reply: `{"streams":[{"id","start","tokens","times"}],"ms"}`.
/// Error lines never get here ([`Helper::exchange`] turns them into errors).
#[derive(Debug, Deserialize)]
pub struct DecodeReply {
    #[serde(default)]
    pub streams: Vec<StreamReply>,
}

pub fn parse_decode(reply: serde_json::Value) -> Result<DecodeReply> {
    let text = reply.to_string();
    serde_json::from_value(reply).with_context(|| format!("unreadable speech-helper reply: {text}"))
}

/// Whether the streaming worker was cancelled since `generation` was read.
fn cancelled_since(generation: u64) -> bool {
    super::worker::CANCEL_GENERATION.load(Ordering::SeqCst) != generation
}

pub struct NativeRecognizer {
    compute: Compute,
    helper: Helper,
    results: Mutex<HashMap<StreamId, Hypothesis>>,
    next_id: AtomicU32,
}

impl NativeRecognizer {
    /// Start the helper's live role on the FluidAudio models directory.
    /// Returns at once: the models load (seconds; a GPU compiles its shaders
    /// on every launch) while the recording starts. Audio sent meanwhile is
    /// held by [`Helper`], and the first decode waits for the load
    /// ([`Recognizer::wait_ready`]).
    pub fn spawn(models_dir: &Path, compute: Compute) -> Result<Self> {
        let helper = Helper::spawn(Role::Live, models_dir, compute)?;
        // A cancel of the worker using this engine (Stop while the model
        // compiles or a decode hangs) ends any wait on the helper at once;
        // the helper is broken afterwards and killed when dropped.
        let generation = super::worker::CANCEL_GENERATION.load(Ordering::SeqCst);
        helper.cancel_when(move || cancelled_since(generation));
        Ok(Self {
            compute,
            helper,
            results: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
        })
    }

    /// A reply-less frame. A failure breaks the helper, and the next decode
    /// reports it.
    fn send(&self, op: u8, stream: StreamId, payload: &[u8]) {
        let _ = self.helper.send(op, stream, payload);
    }
}

impl Recognizer for NativeRecognizer {
    fn device(&self) -> &'static str {
        self.compute.arg()
    }

    fn wait_ready(&self) -> Result<()> {
        self.helper.wait_ready(LOAD_TIMEOUT).map(|_| ())
    }

    fn create_stream(&self, sample_rate: u32) -> Result<StreamId> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Hypothesis::default());
        self.send(op::OPEN, id, &sample_rate.to_le_bytes());
        Ok(id)
    }

    fn accept(&self, id: StreamId, _sample_rate: u32, samples: &[f32]) {
        self.send(op::AUDIO, id, &audio_payload(samples));
    }

    fn input_finished(&self, id: StreamId) {
        self.send(op::FINISH, id, &[]);
    }

    fn decode(&self, _ids: &[StreamId]) -> Result<()> {
        // The helper decodes every open stream; ones with nothing ready are
        // a no-op there. The first decode waits for the models to load.
        let reply = parse_decode(self.helper.exchange(op::DECODE, 0, &[], DECODE_TIMEOUT)?)?;
        let mut results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        for stream in reply.streams {
            if let Some(hypothesis) = results.get_mut(&stream.id) {
                *hypothesis = Hypothesis {
                    tokens: stream.tokens,
                    times: stream.times,
                    start: stream.start,
                };
            }
        }
        Ok(())
    }

    fn result(&self, id: StreamId) -> Option<Hypothesis> {
        let results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        results.get(&id).cloned()
    }

    fn reset(&self, id: StreamId) {
        self.send(op::RESET, id, &[]);
        if let Some(hypothesis) = self
            .results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&id)
        {
            hypothesis.tokens.clear();
            hypothesis.times.clear();
        }
    }

    fn close(&self, id: StreamId) {
        self.send(op::CLOSE, id, &[]);
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_replies_carry_each_streams_hypothesis() {
        let reply = parse_decode(
            serde_json::from_str(
                r#"{"streams":[{"id":2,"start":1.5,"tokens":[" Hello",","],"times":[0.4,0.48]}],"ms":3.1}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reply.streams[0].id, 2);
        assert_eq!(reply.streams[0].tokens, vec![" Hello", ","]);
        assert_eq!(reply.streams[0].times, vec![0.4, 0.48]);
        assert_eq!(reply.streams[0].start, 1.5);
    }

    #[test]
    fn a_decode_reply_without_streams_has_none() {
        let reply = parse_decode(serde_json::json!({ "ms": 0.2 })).unwrap();
        assert!(reply.streams.is_empty());
        assert!(parse_decode(serde_json::json!({ "streams": "nope" })).is_err());
    }

    #[test]
    fn a_cancel_after_spawn_is_seen_by_the_engine() {
        let generation = super::super::worker::CANCEL_GENERATION.load(Ordering::SeqCst);
        assert!(!cancelled_since(generation));
        super::super::worker::CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst);
        assert!(cancelled_since(generation));
    }

    /// Against the real helper and models: set `NEMOTRON_COREML_MODEL_DIR`
    /// to the FluidAudio models directory (holding
    /// `nemotron-speech-streaming-en-0.6b-coreml/`), build `speech-helper`,
    /// then run with `--ignored`.
    #[test]
    #[ignore]
    fn the_helper_decodes_streams_over_the_pipe() {
        let (Ok(dir), Some(_)) = (
            std::env::var("NEMOTRON_COREML_MODEL_DIR"),
            crate::audio::speech::helper::resolve_helper(),
        ) else {
            return;
        };
        let recognizer = NativeRecognizer::spawn(Path::new(&dir), Compute::NeuralEngine).unwrap();
        let a = recognizer.create_stream(48_000).unwrap();
        let b = recognizer.create_stream(48_000).unwrap();
        // A 440 Hz tone on one stream, silence on the other: decodes cleanly
        // (no words expected) and both streams answer.
        let tone: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.1)
            .collect();
        recognizer.accept(a, 48_000, &tone);
        recognizer.accept(b, 48_000, &vec![0.0; 48_000]);
        recognizer.decode(&[a, b]).unwrap();
        recognizer.input_finished(a);
        recognizer.decode(&[a, b]).unwrap();
        assert!(recognizer.result(a).is_some());
        assert!(recognizer.result(b).is_some());
        recognizer.close(a);
        recognizer.close(b);
    }
}
