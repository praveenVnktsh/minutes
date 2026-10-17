//! Live transcription with a streaming model (Nemotron Speech Streaming 0.6B
//! through sherpa-onnx). See
//! `docs/superpowers/specs/2026-10-16-live-offline-transcription-design.md`.

pub mod commands;
pub mod engine;
pub mod gate;
pub mod model;
pub mod transcriber;
pub mod worker;

pub use worker::start_streaming_transcription_task;
