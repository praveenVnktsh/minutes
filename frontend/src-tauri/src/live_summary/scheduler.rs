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
    /// No window is due before this (a back-off after failures).
    not_before: Duration,
    /// Older segments already dropped from `pending` to bound it.
    trimmed_segments: usize,
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
            not_before: Duration::ZERO,
            trimmed_segments: 0,
        }
    }

    pub fn push(&mut self, segment: Segment) {
        if segment.text.trim().is_empty() {
            return;
        }
        self.pending.push(segment);
        // Bound memory through a long back-off: a window only ever carries
        // the newest MAX_WINDOW_WORDS, so text much older than that is dead.
        if self.pending_words() > MAX_WINDOW_WORDS * 2 {
            self.trim_pending();
        }
    }

    /// Put a failed window's text back, ahead of what arrived since, so the
    /// next window retries it. The next window still carries at most
    /// `MAX_WINDOW_WORDS`, keeping the newest text.
    pub fn requeue(&mut self, window: Window) {
        let mut segments = window.segments;
        segments.append(&mut self.pending);
        self.pending = segments;
        self.trim_pending();
    }

    /// No window before `now + delay`.
    pub fn back_off(&mut self, now: Duration, delay: Duration) {
        self.not_before = now + delay;
    }

    fn trim_pending(&mut self) {
        self.pending.sort_by_key(|segment| segment.start_ms);
        let mut words = self.pending_words();
        while words > MAX_WINDOW_WORDS && self.pending.len() > 1 {
            words -= self.pending[0].words();
            self.pending.remove(0);
            self.trimmed_segments += 1;
        }
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
        if now.saturating_sub(self.last_request_at) < self.cadence || now < self.not_before {
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
        let mut dropped_segments = std::mem::take(&mut self.trimmed_segments);
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

    /// Everything pending, for a scheduler of another kind.
    fn take_pending(&mut self) -> Vec<Segment> {
        self.trimmed_segments = 0;
        std::mem::take(&mut self.pending)
    }
}

/// Pause-aligned windows (the fine-tuned live notes model's): a window closes
/// at the first pause in speech or change of speaker once it is
/// `PAUSE_MIN_CLOSE_MS` long, and is closed before a segment would stretch it
/// past `PAUSE_MAX_MS`. A port of the training data's windows
/// (minutes-distill `simulate_scheduler.py --mode pause`, defaults).
pub const PAUSE_MIN_CLOSE_MS: i64 = 20_000;
pub const PAUSE_MAX_MS: i64 = 45_000;
/// A gap between segments this long is a pause.
pub const PAUSE_GAP_MS: i64 = 800;
/// A window has at least this many words; shorter text waits for more.
pub const PAUSE_MIN_WORDS: usize = 12;
/// The next segment is not known yet, so a window long enough to close also
/// closes when nothing new has arrived for this long (the speech paused).
pub const PAUSE_IDLE: Duration = Duration::from_secs(3);
/// Notes asked for from a pause-aligned window (the training data's budget).
pub const ENTRIES_PER_PAUSE_WINDOW: usize = 1;

/// The fine-tuned model's scheduler: closes pause-aligned windows as speech
/// arrives and hands them out one at a time, in order. Windows closed while
/// a request is in flight wait their turn rather than merge: the model
/// learned on one window at a time.
#[derive(Debug, Default)]
pub struct PauseScheduler {
    /// The window being collected, by audio time.
    current: Vec<Segment>,
    /// The speaker of the last segment pushed.
    last_speaker: Option<Speaker>,
    last_received: Duration,
    ready: std::collections::VecDeque<Window>,
    in_flight: bool,
    not_before: Duration,
}

impl PauseScheduler {
    pub fn push(&mut self, segment: Segment) {
        if segment.text.trim().is_empty() {
            return;
        }
        if !self.current.is_empty() {
            let last_end = self.last_end();
            let pause = segment.start_ms - last_end >= PAUSE_GAP_MS
                || self.last_speaker != Some(segment.speaker);
            let too_long = segment.end_ms - self.current[0].start_ms > PAUSE_MAX_MS;
            if (self.closable() && pause) || (self.enough_words() && too_long) {
                self.close();
            }
        }
        self.last_speaker = Some(segment.speaker);
        self.last_received = segment.received_at;
        let at = self
            .current
            .partition_point(|s| (s.start_ms, s.end_ms) <= (segment.start_ms, segment.end_ms));
        self.current.insert(at, segment);
    }

    fn last_end(&self) -> i64 {
        self.current.iter().map(|s| s.end_ms).max().unwrap_or(0)
    }

    fn enough_words(&self) -> bool {
        self.current.iter().map(Segment::words).sum::<usize>() >= PAUSE_MIN_WORDS
    }

    /// Long enough and wordy enough to close at the next pause.
    fn closable(&self) -> bool {
        !self.current.is_empty()
            && self.last_end() - self.current[0].start_ms >= PAUSE_MIN_CLOSE_MS
            && self.enough_words()
    }

    fn close(&mut self) {
        let segments = std::mem::take(&mut self.current);
        let start_ms = segments[0].start_ms;
        let end_ms = segments
            .iter()
            .map(|s| s.end_ms.max(s.start_ms))
            .max()
            .unwrap_or(start_ms);
        self.ready.push_back(Window {
            segments,
            start_ms,
            end_ms,
            max_entries: ENTRIES_PER_PAUSE_WINDOW,
            dropped_segments: 0,
        });
    }

    /// The next window, in order, once none is in flight.
    pub fn poll(&mut self, now: Duration, paused: bool) -> Option<Window> {
        if self.closable() && now.saturating_sub(self.last_received) >= PAUSE_IDLE {
            self.close();
        }
        if self.in_flight || paused || now < self.not_before {
            return None;
        }
        self.take()
    }

    /// At stop: the window being collected closes if it has the words, and
    /// the windows still waiting go out one by one.
    pub fn flush(&mut self, _now: Duration) -> Option<Window> {
        if self.enough_words() {
            self.close();
        }
        if self.in_flight {
            return None;
        }
        self.take()
    }

    fn take(&mut self) -> Option<Window> {
        let window = self.ready.pop_front()?;
        self.in_flight = true;
        Some(window)
    }

    /// The request for the last window finished (successfully or not).
    pub fn complete(&mut self) {
        self.in_flight = false;
    }

    /// A failed window goes back to the front of the line.
    pub fn requeue(&mut self, window: Window) {
        self.ready.push_front(window);
    }

    pub fn clear(&mut self) {
        self.current.clear();
        self.ready.clear();
    }
}

/// The worker's scheduler: timed windows (`WindowScheduler`) for every model
/// but the fine-tuned live notes model, which gets pause-aligned ones.
#[derive(Debug)]
pub enum Scheduler {
    Timed(WindowScheduler),
    Pauses(PauseScheduler),
}

impl Scheduler {
    pub fn is_pauses(&self) -> bool {
        matches!(self, Self::Pauses(_))
    }

    /// Switch to pause-aligned windows, keeping the text not yet sent.
    pub fn to_pauses(&mut self, mut segments: Vec<Segment>) {
        if let Self::Timed(timed) = self {
            segments.extend(timed.take_pending());
            let mut pauses = PauseScheduler::default();
            segments.sort_by_key(|s| (s.start_ms, s.end_ms));
            for segment in segments {
                pauses.push(segment);
            }
            *self = Self::Pauses(pauses);
        }
    }

    pub fn push(&mut self, segment: Segment) {
        match self {
            Self::Timed(s) => s.push(segment),
            Self::Pauses(s) => s.push(segment),
        }
    }

    pub fn poll(&mut self, now: Duration, paused: bool) -> Option<Window> {
        match self {
            Self::Timed(s) => s.poll(now, paused),
            Self::Pauses(s) => s.poll(now, paused),
        }
    }

    pub fn flush(&mut self, now: Duration) -> Option<Window> {
        match self {
            Self::Timed(s) => s.flush(now),
            Self::Pauses(s) => s.flush(now),
        }
    }

    pub fn complete(&mut self) {
        match self {
            Self::Timed(s) => s.complete(),
            Self::Pauses(s) => s.complete(),
        }
    }

    pub fn back_off(&mut self, now: Duration, delay: Duration) {
        match self {
            Self::Timed(s) => s.back_off(now, delay),
            Self::Pauses(s) => s.not_before = now + delay,
        }
    }

    pub fn requeue(&mut self, window: Window) {
        match self {
            Self::Timed(s) => s.requeue(window),
            Self::Pauses(s) => s.requeue(window),
        }
    }

    pub fn clear(&mut self) {
        match self {
            Self::Timed(s) => s.clear(),
            Self::Pauses(s) => s.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_window_is_retried_with_the_new_text_up_to_the_cap() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 400, 0));
        let failed = scheduler.poll(secs(60), false).unwrap();
        scheduler.complete();
        scheduler.push(segment(70, 500, 70));
        scheduler.requeue(failed);
        scheduler.push(segment(100, 500, 100));
        let retry = scheduler.poll(secs(120), false).unwrap();
        // 1,400 words pending: the oldest (the failed window's) goes first.
        assert_eq!(retry.words(), 1000);
        assert_eq!(retry.dropped_segments, 1);
        assert_eq!(retry.start_ms, 70_000);
    }

    #[test]
    fn back_off_delays_the_next_window() {
        let mut scheduler = WindowScheduler::new(secs(0));
        scheduler.push(segment(0, 100, 0));
        scheduler.back_off(secs(0), secs(240));
        assert!(scheduler.poll(secs(60), false).is_none());
        assert!(scheduler.poll(secs(239), false).is_none());
        assert!(scheduler.poll(secs(240), false).is_some());
    }

    #[test]
    fn pending_text_stays_bounded_while_nothing_is_sent() {
        let mut scheduler = WindowScheduler::new(secs(0));
        for i in 0..100 {
            scheduler.push(segment(i * 10, 100, i as u64 * 10));
        }
        assert!(scheduler.pending_words() <= MAX_WINDOW_WORDS * 2);
        let window = scheduler.poll(secs(2000), false).unwrap();
        assert_eq!(window.words(), MAX_WINDOW_WORDS);
        assert_eq!(window.dropped_segments, 88);
    }

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
