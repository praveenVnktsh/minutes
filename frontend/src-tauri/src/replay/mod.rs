//! Pipeline replay harness (developer tool).
//!
//! Feeds a recorded meeting through the real live pipeline (streaming live
//! transcription, the live summary timeline, Copilot with scripted asks), then
//! the final pass, diarization and the summary, recording every UI event to
//! `events.jsonl` and scoring the run against the live storyboard.
//!
//! - [`events`], [`script`] and [`report`] are pure and always compiled (their
//!   tests run in PR Check).
//! - [`runner`] drives the app headless on Tauri's mock runtime and is only
//!   built with the `dev-replay` feature:
//!
//! ```text
//! cargo run --release -p meetily --features dev-replay --example replay -- \
//!     --audio "/path/to/meeting/audio.mp4"
//! ```
//!
//! Outputs stay on this machine, under the app data folder's `replays/`.
//! Recordings are private: never commit or upload them, their event logs or
//! their reports. The viewer is `site/replay/`, which reads the files locally
//! in the browser.

pub mod events;
pub mod report;
pub mod script;
pub mod trace;

#[cfg(feature = "dev-replay")]
pub mod runner;
