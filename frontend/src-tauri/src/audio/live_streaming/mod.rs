//! Live transcription with a streaming model: Nemotron Speech Streaming 0.6B
//! on Core ML, run by FluidAudio in the `speech-helper` sidecar
//! (`audio/speech/`). See
//! `docs/superpowers/specs/2026-10-16-live-offline-transcription-design.md`.

#[cfg(test)]
mod bench;
pub mod commands;
pub mod engine;
pub mod gate;
pub mod native;
pub mod recognizer;
pub mod transcriber;
pub mod worker;

pub use worker::start_streaming_transcription_task;
