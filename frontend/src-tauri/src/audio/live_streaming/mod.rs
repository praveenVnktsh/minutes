//! Live transcription with a streaming model (Nemotron Speech Streaming 0.6B
//! through sherpa-onnx). See
//! `docs/superpowers/specs/2026-10-16-live-offline-transcription-design.md`.
//!
//! sherpa-onnx is a macOS-only dependency (Cargo.toml), as it already was for
//! diarization: its prebuilt static library carries its own ONNX Runtime,
//! which on Windows/Linux would sit next to `ort`'s runtime (`load-dynamic`
//! on Windows), the two-runtimes setup that aborts the process at exit. On
//! other platforms the recognizer is a stub that never loads, live
//! transcription uses the chunked engine, and the settings report Nemotron as
//! unsupported. The model download, settings and commands exist everywhere,
//! so the frontend sees the same command surface.

pub mod commands;
pub mod engine;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod gate;
pub mod model;
#[cfg(target_os = "macos")]
pub mod transcriber;
pub mod worker;

pub use worker::start_streaming_transcription_task;

/// Whether this build can run the Nemotron streaming model.
pub const SUPPORTED: bool = cfg!(target_os = "macos");
