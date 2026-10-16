//! When the live summary asks the model for entries, and over which text.
//!
//! Pure: time is passed in as the session's elapsed time, so the rules are
//! testable without a clock. The worker owns one scheduler per recording.

use std::time::Duration;

/// How long after the last request started the next window is due.
pub const CADENCE: Duration = Duration::from_secs(60);
/// A window needs at least this many words of new speech to be worth a request.
pub const MIN_WORDS: usize = 25;
/// The most text one request carries. A backlog beyond it keeps the newest text.
pub const MAX_WINDOW_WORDS: usize = 1200;
/// Text too short for a window carries over, but not forever: once its oldest
/// segment is this old it is dropped, so a stray "okay" does not stretch the
/// next window back by minutes.
pub const MAX_IDLE_CARRY: Duration = Duration::from_secs(300);
/// Entries asked for from a normal window.
pub const ENTRIES_PER_WINDOW: usize = 2;
/// Entries allowed from a merged window (a backlog spanning well over a cadence).
pub const ENTRIES_PER_MERGED_WINDOW: usize = 3;

/// Who spoke, as the timeline names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// The person recording (the microphone).
    You,
    /// Remote participants (system audio), possibly several people.
    Others,
}

impl Speaker {
    /// From a transcript segment's source (`mic` / `system`).
    pub fn from_source(source: &str) -> Self {
        match source {
            "mic" | "microphone" => Self::You,
            _ => Self::Others,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::You => "You",
            Self::Others => "Others",
        }
    }
}

/// A finalized live transcript segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Audio time from the recording start.
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Speaker,
    pub text: String,
    /// Session time the segment arrived at.
    pub received_at: Duration,
}

impl Segment {
    fn words(&self) -> usize {
        word_count(&self.text)
    }
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
}

/// The text one request summarizes.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub segments: Vec<Segment>,
    pub start_ms: i64,
    pub end_ms: i64,
    /// How many entries the model may return for it.
    pub max_entries: usize,
    /// Older segments dropped because the backlog was over `MAX_WINDOW_WORDS`.
    pub dropped_segments: usize,
}

impl Window {
    pub fn words(&self) -> usize {
        self.segments.iter().map(Segment::words).sum()
    }
}

#[derive(Debug)]
pub struct WindowScheduler {
    pending: Vec<Segment>,
    last_request_at: Duration,
    in_flight: bool,
    cadence: Duration,
}

impl WindowScheduler {
    /// `now` is when the session started; the first window is due a cadence later.
    pub fn new(now: Duration) -> Self {
        Self::with_cadence(now, CADENCE)
    }

    /// A scheduler with its own cadence (tests run the worker on milliseconds).
    pub fn with_cadence(now: Duration, cadence: Duration) -> Self {
        Self {
            pending: Vec::new(),
            last_request_at: now,
            in_flight: false,
            cadence,
        }
    }

    pub fn push(&mut self, segment: Segment) {
        if segment.text.trim().is_empty() {
            return;
        }
        self.pending.push(segment);
    }

    pub fn pending_words(&self) -> usize {
        self.pending.iter().map(Segment::words).sum()
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// The window due at `now`, if any. Nothing is due while a request is in
    /// flight (its text keeps collecting and merges into the next window),
    /// while paused, before the cadence, or without enough speech.
    pub fn poll(&mut self, now: Duration, paused: bool) -> Option<Window> {
        if self.in_flight || paused {
            return None;
        }
        if now.saturating_sub(self.last_request_at) < self.cadence {
            return None;
        }
        self.drop_stale_short_text(now);
        if self.pending_words() < MIN_WORDS {
            return None;
        }
        Some(self.take_window(now))
    }

    /// The last window when the recording stops: the cadence no longer
    /// applies, the speech threshold still does.
    pub fn flush(&mut self, now: Duration) -> Option<Window> {
        if self.in_flight || self.pending_words() < MIN_WORDS {
            return None;
        }
        Some(self.take_window(now))
    }

    /// The request for the last window finished (successfully or not).
    pub fn complete(&mut self) {
        self.in_flight = false;
    }

    /// Forget pending text (the feature was turned off mid-recording).
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    fn drop_stale_short_text(&mut self, now: Duration) {
        let Some(oldest) = self.pending.first() else {
            return;
        };
        if self.pending_words() < MIN_WORDS
            && now.saturating_sub(oldest.received_at) > MAX_IDLE_CARRY
        {
            self.pending.clear();
        }
    }

    fn take_window(&mut self, now: Duration) -> Window {
        let mut segments = std::mem::take(&mut self.pending);
        segments.sort_by_key(|segment| segment.start_ms);

        // Coalesce a long backlog into one window of the newest text.
        let mut words: usize = segments.iter().map(Segment::words).sum();
        let mut dropped_segments = 0;
        while words > MAX_WINDOW_WORDS && segments.len() > 1 {
            words -= segments[0].words();
            segments.remove(0);
            dropped_segments += 1;
        }

        let start_ms = segments.iter().map(|s| s.start_ms).min().unwrap_or(0);
        let end_ms = segments
            .iter()
            .map(|s| s.end_ms.max(s.start_ms))
            .max()
            .unwrap_or(start_ms);
        // Spans are audio time, so "merged" is judged against the real cadence.
        let merged_span_ms = (CADENCE.as_millis() as i64) * 3 / 2;
        let max_entries = if end_ms - start_ms > merged_span_ms {
            ENTRIES_PER_MERGED_WINDOW
        } else {
            ENTRIES_PER_WINDOW
        };

        self.in_flight = true;
        self.last_request_at = now;
        Window {
            segments,
            start_ms,
            end_ms,
            max_entries,
            dropped_segments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn words(n: usize) -> String {
        (0..n)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn segment(start_s: i64, n_words: usize, received_s: u64) -> Segment {
        Segment {
            start_ms: start_s * 1000,
            end_ms: start_s * 1000 + 4000,
            speaker: Speaker::Others,
            text: words(n_words),
            received_at: secs(received_s),
        }
    }

    #[test]
    fn speakers_map_from_sources() {
        assert_eq!(Speaker::from_source("mic"), Speaker::You);
        assert_eq!(Speaker::from_source("system"), Speaker::Others);
        assert_eq!(Speaker::You.label(), "You");
    }

    #[test]
    fn word_count_ignores_punctuation_only_tokens() {
        assert_eq!(word_count("Okay — so, we ship it . "), 5);
    }

    #[test]
    fn nothing_is_due_before_the_cadence() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 100, 5));
        assert!(scheduler.poll(secs(59), false).is_none());
        assert!(scheduler.poll(secs(60), false).is_some());
    }

    #[test]
    fn silence_makes_no_window() {
        let mut scheduler = WindowScheduler::new(secs(0));
        assert!(scheduler.poll(secs(120), false).is_none());
        assert!(!scheduler.in_flight());
    }

    #[test]
    fn short_text_waits_for_more_speech() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, MIN_WORDS - 1, 5));
        assert!(scheduler.poll(secs(60), false).is_none());
        scheduler.push(segment(70, 1, 75));
        let window = scheduler.poll(secs(80), false).unwrap();
        assert_eq!(window.segments.len(), 2);
        assert_eq!(window.start_ms, 0);
    }

    #[test]
    fn stale_short_text_is_dropped() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 3, 1));
        assert!(scheduler.poll(secs(400), false).is_none());
        assert_eq!(scheduler.pending_words(), 0);
    }

    #[test]
    fn paused_recordings_schedule_nothing() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 100, 5));
        assert!(scheduler.poll(secs(90), true).is_none());
        assert!(scheduler.poll(secs(91), false).is_some());
    }

    #[test]
    fn one_request_in_flight_and_late_text_merges_into_the_next_window() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 40, 5));
        let first = scheduler.poll(secs(60), false).unwrap();
        assert_eq!(first.segments.len(), 1);
        assert!(scheduler.in_flight());

        // Two more windows' worth arrive while the request is slow.
        scheduler.push(segment(65, 40, 66));
        scheduler.push(segment(130, 40, 131));
        assert!(scheduler.poll(secs(125), false).is_none());
        assert!(scheduler.poll(secs(185), false).is_none());

        scheduler.complete();
        let merged = scheduler.poll(secs(186), false).unwrap();
        assert_eq!(merged.segments.len(), 2);
        assert_eq!(merged.start_ms, 65_000);
        assert_eq!(merged.end_ms, 134_000);
    }

    #[test]
    fn a_window_spanning_well_over_a_cadence_may_return_more_entries() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 30, 1));
        assert_eq!(
            scheduler.poll(secs(60), false).unwrap().max_entries,
            ENTRIES_PER_WINDOW
        );
        scheduler.complete();

        scheduler.push(segment(60, 30, 61));
        scheduler.push(segment(200, 30, 201));
        assert_eq!(
            scheduler.poll(secs(210), false).unwrap().max_entries,
            ENTRIES_PER_MERGED_WINDOW
        );
    }

    #[test]
    fn a_long_backlog_coalesces_to_the_newest_text() {
        let mut scheduler = WindowScheduler::new(secs(0));
        for i in 0..10 {
            scheduler.push(segment(i * 20, 200, i as u64));
        }
        let window = scheduler.poll(secs(60), false).unwrap();
        assert!(window.words() <= MAX_WINDOW_WORDS);
        assert_eq!(window.dropped_segments, 4);
        assert_eq!(window.start_ms, 80_000);
        assert_eq!(scheduler.pending_words(), 0);
    }

    #[test]
    fn segments_are_ordered_by_audio_time() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(30, 20, 1));
        scheduler.push(segment(10, 20, 2));
        let window = scheduler.poll(secs(60), false).unwrap();
        assert_eq!(window.segments[0].start_ms, 10_000);
    }

    #[test]
    fn flush_ignores_the_cadence_but_not_the_word_threshold() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 10, 1));
        assert!(scheduler.flush(secs(5)).is_none());
        scheduler.push(segment(5, 30, 6));
        assert!(scheduler.flush(secs(7)).is_some());
    }

    #[test]
    fn empty_segments_are_ignored() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(Segment {
            text: "   ".into(),
            ..segment(0, 0, 0)
        });
        assert_eq!(scheduler.pending_words(), 0);
        assert!(scheduler.flush(secs(1)).is_none());
    }
}
