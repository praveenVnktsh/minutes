// audio/transcription/dedup.rs
//
// Duplicate suppression for live transcript segments.
//
// The audio pipeline runs one Silero VAD processor per capture source
// (microphone and system audio) over the *same* aligned mixing windows, so both
// VADs share a single recording timeline. When the same speech reaches both
// capture paths - a remote participant heard over the speakers and also picked
// up by an open microphone, a headset that loops back, a mixed monitor path -
// each source produces its own speech segment, each is transcribed, and the
// worker emits two `transcript-update` events with the same words but different
// `sequence_id`s. The UI and the recording saver both deduplicate by
// `sequence_id` only, so those land as two blocks of identical text.
//
// This module holds the rule that decides, before anything is emitted, whether
// a freshly transcribed segment is one of those echoes.

use log::debug;
use std::collections::VecDeque;

/// How far back (in seconds of recording time) a segment is kept in the history.
///
/// The rule only ever matches segments whose audio windows overlap, so history
/// older than the longest segment that could still overlap the newest one is
/// dead weight. The pipeline force-flushes a live segment at 15s
/// (`LIVE_MAX_SEGMENT_MS` in `audio/pipeline.rs`), so 15s is the longest window
/// we can see; doubling that leaves room for transcription workers completing
/// out of order and still bounds the history to a handful of entries.
const RETENTION_HORIZON_SECS: f64 = 30.0;

/// Hard backstop on retained entries, independent of the time horizon.
///
/// Pathologically short segments (rapid VAD flapping) could otherwise pack many
/// entries into the horizon. 128 entries keeps the per-candidate scan trivially
/// cheap - a two-hour meeting costs the same as the first minute - while still
/// covering far more overlapping segments than the pipeline can realistically
/// produce inside 30s.
const MAX_HISTORY_ENTRIES: usize = 128;

/// Slack applied to interval comparisons, in seconds.
///
/// Windows that merely touch (`a.end == b.start`) are adjacent speech, not the
/// same speech, so they must not count as overlapping. Requiring the overlap to
/// exceed 1ms also absorbs float rounding from the ms -> s conversion done when
/// a VAD segment becomes an audio chunk.
const OVERLAP_EPSILON_SECS: f64 = 1e-3;

/// The verdict for one candidate segment.
#[derive(Debug, Clone, PartialEq)]
pub enum DedupVerdict {
    /// Nothing in the retained history says these words were already emitted
    /// over this stretch of the timeline. The candidate has been recorded, so
    /// later echoes of it will be caught.
    Emit,
    /// The candidate repeats an earlier segment whose audio window overlaps it.
    /// The caller should skip it entirely - no event, no sequence id.
    Duplicate {
        /// Start of the already-emitted segment, in seconds from recording start.
        previous_start: f64,
        /// End of the already-emitted segment, in seconds from recording start.
        previous_end: f64,
    },
}

impl DedupVerdict {
    /// Convenience predicate for callers that only care whether to emit.
    pub fn is_duplicate(&self) -> bool {
        matches!(self, DedupVerdict::Duplicate { .. })
    }
}

/// One already-emitted segment, kept only as long as it could still overlap.
#[derive(Debug, Clone)]
struct EmittedSegment {
    normalized_text: String,
    start: f64,
    end: f64,
}

/// Suppresses transcript segments that repeat words already emitted over an
/// overlapping stretch of the recording timeline.
///
/// # The rule
///
/// A candidate is a duplicate of a retained segment when **both** hold:
///
/// 1. their texts match after normalization (trim, case-fold, collapse internal
///    whitespace, strip leading/trailing punctuation), and
/// 2. their audio windows `[start, end)` overlap by more than
///    [`OVERLAP_EPSILON_SECS`].
///
/// Identical text at a *non-overlapping* time - a phrase the speaker genuinely
/// repeats a minute later - is emitted normally. That is the whole point of
/// pairing the text match with the interval test.
///
/// # The capture source is deliberately not part of the key
///
/// The bug this exists to fix is the *same* words arriving from "mic" and
/// "system" in the same window. Keying history by source would file those two
/// copies separately and let both through, defeating the fix. Only text and
/// time participate in the match.
///
/// # Cost
///
/// History is bounded by both [`RETENTION_HORIZON_SECS`] and
/// [`MAX_HISTORY_ENTRIES`], so a candidate is compared against a small,
/// constant-bounded number of entries no matter how long the meeting runs.
/// A candidate is always checked against *all* retained entries, not just the
/// most recent one, because transcription workers finish out of order.
pub struct TranscriptDeduper {
    /// Ordered oldest-first by insertion; eviction pops from the front.
    history: VecDeque<EmittedSegment>,
    /// Latest segment end seen so far, used as the eviction reference point so
    /// a late-arriving older segment cannot prune newer history.
    timeline_head: f64,
}

impl Default for TranscriptDeduper {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptDeduper {
    /// Create an empty deduplicator.
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            timeline_head: f64::NEG_INFINITY,
        }
    }

    /// Judge one candidate segment and, when it is not a duplicate, record it.
    ///
    /// `audio_start_time` / `audio_end_time` are seconds from the start of the
    /// recording - the same timeline both VAD processors are driven from.
    /// Returns [`DedupVerdict::Emit`] when the caller should emit the segment,
    /// or [`DedupVerdict::Duplicate`] naming the window it repeats.
    ///
    /// Text that normalizes to nothing (empty or punctuation-only) is reported
    /// as `Emit` without being recorded; the worker already filters that case,
    /// and it must never become a key that swallows later segments.
    pub fn check_and_record(
        &mut self,
        text: &str,
        audio_start_time: f64,
        audio_end_time: f64,
    ) -> DedupVerdict {
        let normalized = normalize_text(text);
        if normalized.is_empty() {
            return DedupVerdict::Emit;
        }

        // Defensive: a reversed or non-finite window cannot be reasoned about,
        // so let it through rather than matching everything or nothing.
        if !audio_start_time.is_finite() || !audio_end_time.is_finite() {
            return DedupVerdict::Emit;
        }
        let (start, end) = if audio_start_time <= audio_end_time {
            (audio_start_time, audio_end_time)
        } else {
            (audio_end_time, audio_start_time)
        };

        self.timeline_head = self.timeline_head.max(end);
        self.evict_stale();

        if let Some(previous) = self
            .history
            .iter()
            .find(|entry| entry.normalized_text == normalized && overlaps(entry.start, entry.end, start, end))
        {
            let verdict = DedupVerdict::Duplicate {
                previous_start: previous.start,
                previous_end: previous.end,
            };
            debug!(
                "🔁 Duplicate transcript segment suppressed: \"{}\" [{:.2}s-{:.2}s] overlaps [{:.2}s-{:.2}s]",
                normalized, start, end, previous.start, previous.end
            );
            return verdict;
        }

        self.history.push_back(EmittedSegment {
            normalized_text: normalized,
            start,
            end,
        });
        while self.history.len() > MAX_HISTORY_ENTRIES {
            self.history.pop_front();
        }

        DedupVerdict::Emit
    }

    /// Forget everything, so a new recording session starts on a clean timeline.
    pub fn reset(&mut self) {
        self.history.clear();
        self.timeline_head = f64::NEG_INFINITY;
    }

    /// Number of retained segments; exposed for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// True when no segment is retained.
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Drop entries that ended more than [`RETENTION_HORIZON_SECS`] before the
    /// newest point of the timeline seen so far - they can no longer overlap
    /// anything that arrives from here on.
    fn evict_stale(&mut self) {
        let cutoff = self.timeline_head - RETENTION_HORIZON_SECS;
        self.history.retain(|entry| entry.end >= cutoff);
    }
}

/// True when `[a_start, a_end)` and `[b_start, b_end)` share more than
/// [`OVERLAP_EPSILON_SECS`] of the timeline. Windows that only touch at a
/// boundary do not overlap.
fn overlaps(a_start: f64, a_end: f64, b_start: f64, b_end: f64) -> bool {
    let overlap = a_end.min(b_end) - a_start.max(b_start);
    overlap > OVERLAP_EPSILON_SECS
}

/// Normalize transcript text for comparison.
///
/// Case-folds, collapses every run of whitespace to a single space, and strips
/// punctuation from the two ends of the result - the details transcribers vary
/// on between two passes over the same audio ("Okay, let's start." vs
/// "okay, let's start"). Punctuation *inside* the phrase is kept, so genuinely
/// different sentences stay different.
fn normalize_text(text: &str) -> String {
    let collapsed: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    collapsed
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deduper() -> TranscriptDeduper {
        TranscriptDeduper::new()
    }

    #[test]
    fn collapses_same_words_from_mic_and_system_in_one_window() {
        // The bug: one utterance reaches both capture paths over the same
        // aligned windows, so both VADs emit it on the same timeline.
        let mut d = deduper();
        assert_eq!(d.check_and_record("Let's get started", 10.0, 12.5), DedupVerdict::Emit);

        let verdict = d.check_and_record("Let's get started", 10.0, 12.5);
        assert!(verdict.is_duplicate());
        assert_eq!(
            verdict,
            DedupVerdict::Duplicate {
                previous_start: 10.0,
                previous_end: 12.5
            }
        );
        assert_eq!(d.len(), 1, "a duplicate must not be recorded");
    }

    #[test]
    fn same_text_at_a_later_window_still_emits() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("sounds good", 5.0, 6.0), DedupVerdict::Emit);
        assert_eq!(
            d.check_and_record("sounds good", 20.0, 21.0),
            DedupVerdict::Emit,
            "a genuinely repeated phrase is not a duplicate"
        );
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn case_and_edge_punctuation_differences_collapse() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("Okay, let's start.", 1.0, 3.0), DedupVerdict::Emit);
        assert!(d.check_and_record("  okay, LET'S START  ", 1.2, 3.1).is_duplicate());
        assert!(d.check_and_record("...Okay, let's start!", 1.0, 3.0).is_duplicate());
    }

    #[test]
    fn collapses_internal_whitespace_runs() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("one two three", 0.0, 2.0), DedupVerdict::Emit);
        assert!(d.check_and_record("one\t two\n\nthree", 0.5, 2.5).is_duplicate());
    }

    #[test]
    fn different_text_in_the_same_window_both_emit() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("hello there", 4.0, 6.0), DedupVerdict::Emit);
        assert_eq!(d.check_and_record("general kenobi", 4.0, 6.0), DedupVerdict::Emit);
    }

    #[test]
    fn partial_overlap_counts_as_overlap() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("same words here", 10.0, 14.0), DedupVerdict::Emit);
        assert!(
            d.check_and_record("same words here", 13.0, 17.0).is_duplicate(),
            "one second of shared timeline is an overlap"
        );
    }

    #[test]
    fn containment_counts_as_overlap() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("same words here", 10.0, 20.0), DedupVerdict::Emit);
        assert!(d.check_and_record("same words here", 12.0, 13.0).is_duplicate());
    }

    #[test]
    fn touching_windows_do_not_overlap() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("next slide", 8.0, 10.0), DedupVerdict::Emit);
        assert_eq!(
            d.check_and_record("next slide", 10.0, 12.0),
            DedupVerdict::Emit,
            "a.end == b.start is adjacency, not overlap"
        );
    }

    #[test]
    fn sub_epsilon_overlap_does_not_count() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("thanks", 8.0, 10.0), DedupVerdict::Emit);
        assert_eq!(
            d.check_and_record("thanks", 9.9995, 12.0),
            DedupVerdict::Emit,
            "half a millisecond of shared timeline is float noise"
        );
    }

    #[test]
    fn matches_against_all_retained_entries_not_just_the_last() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("alpha", 10.0, 12.0), DedupVerdict::Emit);
        assert_eq!(d.check_and_record("beta", 10.0, 12.0), DedupVerdict::Emit);
        assert_eq!(d.check_and_record("gamma", 10.0, 12.0), DedupVerdict::Emit);
        // "alpha" is the oldest entry; an out-of-order worker result must still
        // be matched against it.
        assert!(d.check_and_record("alpha", 11.0, 12.5).is_duplicate());
    }

    #[test]
    fn empty_and_punctuation_only_text_is_never_recorded() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("", 1.0, 2.0), DedupVerdict::Emit);
        assert_eq!(d.check_and_record("   ", 1.0, 2.0), DedupVerdict::Emit);
        assert_eq!(d.check_and_record("...", 1.0, 2.0), DedupVerdict::Emit);
        assert!(d.is_empty(), "blank text must not become a history key");
    }

    #[test]
    fn reset_clears_state() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("kickoff", 1.0, 2.0), DedupVerdict::Emit);
        assert!(d.check_and_record("kickoff", 1.0, 2.0).is_duplicate());

        d.reset();
        assert!(d.is_empty());
        assert_eq!(
            d.check_and_record("kickoff", 1.0, 2.0),
            DedupVerdict::Emit,
            "a new session starts on a clean timeline"
        );
    }

    #[test]
    fn history_stays_bounded_by_the_time_horizon() {
        let mut d = deduper();
        // Two hours of one-second segments, one every two seconds.
        for i in 0..3600 {
            let start = i as f64 * 2.0;
            assert_eq!(
                d.check_and_record(&format!("segment number {}", i), start, start + 1.0),
                DedupVerdict::Emit
            );
            assert!(
                d.len() <= MAX_HISTORY_ENTRIES,
                "history exceeded the hard cap at i={}",
                i
            );
        }
        // Only the last 30s of timeline survives: ~15 two-second slots.
        assert!(d.len() <= 16, "expected the horizon to prune, got {}", d.len());
    }

    #[test]
    fn hard_cap_bounds_a_burst_of_segments_inside_one_window() {
        let mut d = deduper();
        // Every segment shares the same tiny window, so the time horizon never
        // evicts anything - the entry cap has to.
        for i in 0..(MAX_HISTORY_ENTRIES * 3) {
            assert_eq!(
                d.check_and_record(&format!("burst {}", i), 100.0, 100.2),
                DedupVerdict::Emit
            );
        }
        assert_eq!(d.len(), MAX_HISTORY_ENTRIES);
    }

    #[test]
    fn stale_entries_stop_matching_once_evicted() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("long gone", 0.0, 1.0), DedupVerdict::Emit);
        // Push the timeline head well past the horizon.
        assert_eq!(d.check_and_record("much later", 100.0, 101.0), DedupVerdict::Emit);
        assert_eq!(d.len(), 1, "the stale entry should have been evicted");
    }

    #[test]
    fn late_arriving_segment_does_not_prune_newer_history() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("recent words", 50.0, 52.0), DedupVerdict::Emit);
        // A worker finishing out of order reports an older window.
        assert_eq!(d.check_and_record("older words", 40.0, 41.0), DedupVerdict::Emit);
        assert!(
            d.check_and_record("recent words", 51.0, 53.0).is_duplicate(),
            "the newer entry must survive an out-of-order candidate"
        );
    }

    #[test]
    fn non_finite_and_reversed_windows_are_handled() {
        let mut d = deduper();
        assert_eq!(d.check_and_record("nan window", f64::NAN, 2.0), DedupVerdict::Emit);
        assert!(d.is_empty(), "a non-finite window is not recorded");

        // A reversed window is normalized, so it still matches its mirror.
        assert_eq!(d.check_and_record("reversed", 6.0, 4.0), DedupVerdict::Emit);
        assert!(d.check_and_record("reversed", 4.0, 6.0).is_duplicate());
    }

    #[test]
    fn normalization_keeps_internal_punctuation_meaningful() {
        assert_eq!(normalize_text("  Hello,   World!  "), "hello, world");
        assert_eq!(normalize_text("Hello, World!"), "hello, world");
        assert_eq!(normalize_text("...???"), "");
        assert_eq!(normalize_text("don't stop"), "don't stop");
    }
}
