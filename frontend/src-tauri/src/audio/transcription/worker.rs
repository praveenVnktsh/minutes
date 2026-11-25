// audio/transcription/worker.rs
//
// Emitting live transcript segments: the `transcript-update` payload, its
// sequence ids, the first-speech event and duplicate filtering. The streaming
// worker (`live_streaming::worker`) is the only producer.

use super::dedup::{DedupVerdict, TranscriptDeduper};
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Runtime};

// Sequence counter for transcript updates
static SEQUENCE_COUNTER: AtomicU64 = AtomicU64::new(0);

// Speech detection flag - reset per recording session
static SPEECH_DETECTED_EMITTED: AtomicBool = AtomicBool::new(false);

/// Reset the speech detected flag for a new recording session
pub fn reset_speech_detected_flag() {
    SPEECH_DETECTED_EMITTED.store(false, Ordering::SeqCst);
    info!(
        "🔍 SPEECH_DETECTED_EMITTED reset to: {}",
        SPEECH_DETECTED_EMITTED.load(Ordering::SeqCst)
    );
}

/// Returns true if the transcript text is non-trivial and should be emitted.
/// Filters empty/whitespace-only text; no confidence gating is applied.
fn should_emit_transcript(text: &str) -> bool {
    !text.trim().is_empty()
}

/// Consult the session's duplicate detector for one transcribed candidate and,
/// only when it is genuinely new, draw the next sequence id for it.
///
/// This is the whole emit-vs-skip decision pulled out of the emit path so it
/// can be unit tested without an app handle: `Some(id)` means "build
/// and emit a `TranscriptUpdate` with this sequence id"; `None` means the
/// candidate was a duplicate, which this function has already logged (with the
/// text, its window and the source it came from) and recorded nothing new for
/// - the caller must not burn a sequence id or emit anything for it.
fn next_sequence_id_if_not_duplicate(
    deduper: &mut TranscriptDeduper,
    sequence_counter: &AtomicU64,
    text: &str,
    source: &str,
    audio_start_time: f64,
    audio_end_time: f64,
) -> Option<u64> {
    match deduper.check_and_record(text, audio_start_time, audio_end_time) {
        DedupVerdict::Emit => Some(sequence_counter.fetch_add(1, Ordering::SeqCst)),
        DedupVerdict::Duplicate {
            previous_start,
            previous_end,
        } => {
            info!(
                "🔁 Skipping duplicate transcript from {} source: \"{}\" [{:.2}s-{:.2}s] overlaps already-emitted [{:.2}s-{:.2}s]",
                source, text, audio_start_time, audio_end_time, previous_start, previous_end
            );
            None
        }
    }
}

/// Emit one finished segment: latch the first-speech event, drop duplicates,
/// draw a sequence id, and send `transcript-update`.
/// Times are on the recording timeline, already shifted by any resume offset.
/// Returns whether the segment was emitted. Used by the streaming worker.
pub(crate) fn emit_final_segment<R: Runtime>(
    app: &AppHandle<R>,
    deduper: &mut TranscriptDeduper,
    text: String,
    source: &str,
    audio_start_time: f64,
    audio_end_time: f64,
    confidence: f32,
) -> bool {
    if !should_emit_transcript(&text) {
        return false;
    }
    if !SPEECH_DETECTED_EMITTED.swap(true, Ordering::SeqCst) {
        let _ = app.emit(
            "speech-detected",
            serde_json::json!({ "message": "Speech activity detected" }),
        );
    }
    let Some(sequence_id) = next_sequence_id_if_not_duplicate(
        deduper,
        &SEQUENCE_COUNTER,
        &text,
        source,
        audio_start_time,
        audio_end_time,
    ) else {
        return false;
    };
    let update = TranscriptUpdate {
        text,
        timestamp: format_current_timestamp(),
        source: source.to_string(),
        sequence_id,
        chunk_start_time: audio_start_time,
        is_partial: false,
        confidence,
        audio_start_time,
        audio_end_time,
        duration: (audio_end_time - audio_start_time).max(0.0),
    };
    if let Err(error) = app.emit("transcript-update", &update) {
        error!("Failed to emit streaming transcript update: {}", error);
        return false;
    }
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TranscriptUpdate {
    pub text: String,
    pub timestamp: String, // Wall-clock time for reference (e.g., "14:30:05")
    pub source: String,
    pub sequence_id: u64,
    pub chunk_start_time: f64, // Legacy field, kept for compatibility
    pub is_partial: bool,
    pub confidence: f32,
    // NEW: Recording-relative timestamps for playback sync
    pub audio_start_time: f64, // Seconds from recording start (e.g., 125.3)
    pub audio_end_time: f64,   // Seconds from recording start (e.g., 128.6)
    pub duration: f64,         // Segment duration in seconds (e.g., 3.3)
}

/// Format current timestamp (wall-clock time)
fn format_current_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();

    let hours = (now.as_secs() / 3600) % 24;
    let minutes = (now.as_secs() / 60) % 60;
    let seconds = now.as_secs() % 60;

    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_acknowledgements() {
        assert!(should_emit_transcript("Yes"));
        assert!(should_emit_transcript("ok"));
    }

    #[test]
    fn drops_empty_and_whitespace_only() {
        assert!(!should_emit_transcript(""));
        assert!(!should_emit_transcript("   "));
    }

    #[test]
    fn first_candidate_is_emitted_with_a_sequence_id() {
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let id = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "let's get started",
            "mic",
            10.0,
            12.5,
        );

        assert_eq!(id, Some(0));
    }

    #[test]
    fn mic_and_system_echo_of_the_same_speech_is_skipped() {
        // The PRA-468 bug: mic and system both carry the same utterance
        // over the same aligned window and each source yields its own
        // segment. The second one must be skipped, not given a sequence id.
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let mic_id = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "let's get started",
            "mic",
            10.0,
            12.5,
        );
        let system_id = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "let's get started",
            "system",
            10.0,
            12.5,
        );

        assert_eq!(mic_id, Some(0));
        assert_eq!(system_id, None, "the overlapping echo must not be emitted");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "a skipped duplicate must not consume a sequence id"
        );
    }

    #[test]
    fn identical_text_at_a_later_non_overlapping_window_still_emits() {
        // A phrase genuinely repeated later in the meeting is not a
        // duplicate - only overlapping windows are.
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let first = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "sounds good",
            "mic",
            5.0,
            6.0,
        );
        let second = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "sounds good",
            "mic",
            20.0,
            21.0,
        );

        assert_eq!(first, Some(0));
        assert_eq!(second, Some(1));
    }

    #[test]
    fn different_text_over_the_same_window_both_emit() {
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let mic = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "hello there",
            "mic",
            4.0,
            6.0,
        );
        let system = next_sequence_id_if_not_duplicate(
            &mut deduper,
            &counter,
            "general kenobi",
            "system",
            4.0,
            6.0,
        );

        assert_eq!(mic, Some(0));
        assert_eq!(system, Some(1));
    }

    #[test]
    fn sequence_ids_are_not_reused_after_a_skip() {
        let mut deduper = TranscriptDeduper::new();
        let counter = AtomicU64::new(0);

        let a = next_sequence_id_if_not_duplicate(&mut deduper, &counter, "one", "mic", 0.0, 1.0);
        let dup =
            next_sequence_id_if_not_duplicate(&mut deduper, &counter, "one", "system", 0.0, 1.0);
        let b = next_sequence_id_if_not_duplicate(&mut deduper, &counter, "two", "mic", 2.0, 3.0);

        assert_eq!(a, Some(0));
        assert_eq!(dup, None);
        assert_eq!(b, Some(1), "the next real segment must not skip ahead to 2");
    }
}
