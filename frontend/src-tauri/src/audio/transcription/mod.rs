// audio/transcription/mod.rs
//
// Live transcription of a recording: the plan (Nemotron streaming through the
// speech helper, or no live text), and emitting its segments.

pub mod dedup;
pub mod live;
pub mod worker;

// Re-export commonly used types
pub use dedup::{DedupVerdict, TranscriptDeduper};
pub use worker::{reset_speech_detected_flag, TranscriptUpdate};
