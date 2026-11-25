//! Every speech model (live Nemotron, Parakeet, Silero VAD, pyannote
//! diarization) runs on Core ML through FluidAudio inside the `speech-helper`
//! sidecar (Swift, `speech-helper/` in the repo).
//!
//! - [`helper`]: the process and its wire protocol (binary frames in, JSON
//!   lines out), shared by the live and batch roles.
//! - [`batch`]: the process-wide batch helper used after a meeting
//!   (transcribe, VAD, diarize).
//! - [`models`]: the model store the helper loads from; the app downloads it,
//!   the helper never touches the network.
//! - [`commands`]: Tauri commands for the model store.

pub mod batch;
pub mod commands;
pub mod helper;
pub mod models;
