// audio/transcription/mod.rs
//
// Transcription module: Provider abstraction, engine management, and worker pool.

pub mod dedup;
pub mod engine;
pub mod live;
pub mod provider;
pub mod worker;

// Re-export commonly used types
pub use dedup::{DedupVerdict, TranscriptDeduper};
pub use engine::{
    get_or_init_transcription_engine, get_or_init_whisper, validate_transcription_model_ready,
    TranscriptionEngine,
};
pub use provider::TranscriptionError;
pub use worker::{reset_speech_detected_flag, start_transcription_task, TranscriptUpdate};
