//! Pure helpers for the streaming worker: a per-source silence gate and the
//! mapping from sherpa's segment times onto the recording timeline.
//!
//! The streaming model wants continuous audio, but decoding silence still
//! costs a full encoder step every 560 ms per source. In most meetings one
//! source is silent much of the time (system audio in an in-person meeting,
//! the microphone while others talk remotely), so the gate stops feeding a
//! source after a stretch of near-silence and starts a fresh stream, with a
//! short pre-roll so the first syllable is kept, when sound comes back.

use std::collections::VecDeque;

/// RMS below this is "quiet". About -56 dBFS: digital silence and a quiet
/// room, well under speech even after the microphone's loudness normalisation.
pub const QUIET_RMS: f32 = 0.0016;
/// Quiet audio kept flowing before the gate closes. Longer than the
/// recognizer's 0.8 s endpoint rule, so a pause normally ends the utterance
/// by itself before the gate does.
pub const CLOSE_AFTER_SECONDS: f64 = 1.5;
/// Audio replayed into a new stream when the gate reopens.
pub const PREROLL_SECONDS: f64 = 0.3;

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateAction {
    /// Gate closed and still quiet: feed nothing.
    Skip,
    /// Gate opens: start a new stream whose zero is `origin_seconds` on the
    /// recording timeline, feed `preroll`, then the window.
    Open {
        origin_seconds: f64,
        preroll: Vec<f32>,
    },
    /// Gate open: feed the window.
    Feed,
    /// Feed the window, then finish the stream: the source has gone quiet.
    FeedThenClose,
}

#[derive(Debug)]
pub struct SilenceGate {
    sample_rate: u32,
    open: bool,
    quiet_samples: u64,
    /// Samples seen so far: the recording position of the next window.
    position: u64,
    preroll: VecDeque<f32>,
}

impl SilenceGate {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            open: false,
            quiet_samples: 0,
            position: 0,
            preroll: VecDeque::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn seconds(&self, samples: u64) -> f64 {
        samples as f64 / self.sample_rate as f64
    }

    pub fn push(&mut self, window: &[f32]) -> GateAction {
        let loud = rms(window) >= QUIET_RMS;
        let start = self.position;
        self.position += window.len() as u64;

        if self.open {
            if loud {
                self.quiet_samples = 0;
                return GateAction::Feed;
            }
            self.quiet_samples += window.len() as u64;
            if self.seconds(self.quiet_samples) >= CLOSE_AFTER_SECONDS {
                self.open = false;
                self.quiet_samples = 0;
                self.preroll.clear();
                return GateAction::FeedThenClose;
            }
            return GateAction::Feed;
        }

        if loud {
            self.open = true;
            self.quiet_samples = 0;
            let preroll: Vec<f32> = self.preroll.drain(..).collect();
            let origin = start.saturating_sub(preroll.len() as u64);
            return GateAction::Open {
                origin_seconds: self.seconds(origin),
                preroll,
            };
        }

        let keep = (PREROLL_SECONDS * self.sample_rate as f64) as usize;
        self.preroll.extend(window.iter().copied());
        while self.preroll.len() > keep {
            self.preroll.pop_front();
        }
        GateAction::Skip
    }

    /// Close without feeding (pause, stop, or dropped backlog). The next loud
    /// window opens a fresh stream.
    pub fn force_close(&mut self) {
        self.open = false;
        self.quiet_samples = 0;
        self.preroll.clear();
    }

    /// Recording position (in samples) of the next window.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Jump to `samples` on the recording timeline (audio was dropped, or live
    /// text started mid-recording). The gate closes; the next loud window
    /// opens a fresh stream there.
    pub fn set_position(&mut self, samples: u64) {
        self.force_close();
        self.position = samples;
    }
}

/// Lead-in before the first token: token times mark where a token was
/// emitted, which trails its onset slightly.
const START_PAD_SECONDS: f64 = 0.15;
/// Tail after the last token, covering its final phone.
const END_PAD_SECONDS: f64 = 0.35;

/// Stream time fed since the last token after which pending text is
/// committed as a finished utterance. Fed time runs up to ~1.2 s ahead of the
/// decoded tokens (560 ms chunks plus look-ahead), so this means roughly
/// 0.6-1.2 s of real silence; measured on the bundled clips, word gaps inside
/// a sentence stay well under it.
pub const COMMIT_AFTER_SILENCE_SECONDS: f64 = 1.8;
/// Pending speech longer than this is split at its widest word gap, so a
/// monologue still reaches the transcript regularly.
pub const MAX_PENDING_SECONDS: f64 = 20.0;

/// Map an utterance onto the recording timeline. `first`/`last` are the
/// stream-relative times of its first and last tokens; `origin` is where the
/// stream's zero lies in the recording.
pub fn segment_bounds(origin: f64, first: f64, last: f64) -> (f64, f64) {
    let start = (origin + first - START_PAD_SECONDS).max(origin).max(0.0);
    let end = (origin + last + END_PAD_SECONDS).max(start + 0.1);
    (start, end)
}

/// How many tokens are committed after this decode step, or `None` to leave
/// them pending.
///
/// sherpa's own endpoint + `reset` was measured to drop words in flight at the
/// boundary, so segmentation happens here instead, on the hypothesis' token
/// times, and the stream is never reset mid-speech:
/// - everything pending is committed once no token has arrived for
///   [`COMMIT_AFTER_SILENCE_SECONDS`] of stream time;
/// - pending speech longer than [`MAX_PENDING_SECONDS`] is cut at the widest
///   gap before a word start (tokens beginning with a space), keeping the
///   last second pending since it may still change.
///
/// `times` are stream-relative token times, `now` the stream time fed so far.
pub fn next_commit(tokens: &[String], times: &[f64], committed: usize, now: f64) -> Option<usize> {
    let len = tokens.len().min(times.len());
    if committed >= len {
        return None;
    }
    if now - times[len - 1] >= COMMIT_AFTER_SILENCE_SECONDS {
        return Some(len);
    }
    if times[len - 1] - times[committed] <= MAX_PENDING_SECONDS {
        return None;
    }
    let settled = times[len - 1] - 1.0;
    (committed + 1..len)
        .filter(|&i| times[i] <= settled && tokens[i].starts_with(' '))
        .max_by(|&a, &b| {
            (times[a] - times[a - 1])
                .total_cmp(&(times[b] - times[b - 1]))
                .then(b.cmp(&a))
        })
}

/// The text of `tokens[range]`, as sherpa would join it.
pub fn join_tokens(tokens: &[String]) -> String {
    tokens.concat().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 1000; // 1 kHz keeps sample arithmetic readable

    fn loud(seconds: f64) -> Vec<f32> {
        vec![0.1; (seconds * SR as f64) as usize]
    }

    fn quiet(seconds: f64) -> Vec<f32> {
        vec![0.0001; (seconds * SR as f64) as usize]
    }

    #[test]
    fn silence_is_never_fed() {
        let mut gate = SilenceGate::new(SR);
        for _ in 0..20 {
            assert_eq!(gate.push(&quiet(0.05)), GateAction::Skip);
        }
        assert!(!gate.is_open());
    }

    #[test]
    fn opening_replays_preroll_and_places_the_stream_on_the_timeline() {
        let mut gate = SilenceGate::new(SR);
        for _ in 0..20 {
            gate.push(&quiet(0.05)); // 1.0 s of silence
        }
        match gate.push(&loud(0.05)) {
            GateAction::Open {
                origin_seconds,
                preroll,
            } => {
                assert_eq!(preroll.len(), (PREROLL_SECONDS * SR as f64) as usize);
                assert!((origin_seconds - (1.0 - PREROLL_SECONDS)).abs() < 1e-9);
            }
            other => panic!("expected Open, got {other:?}"),
        }
        assert!(gate.is_open());
    }

    #[test]
    fn short_pauses_keep_feeding_and_long_ones_close_once() {
        let mut gate = SilenceGate::new(SR);
        assert!(matches!(gate.push(&loud(0.05)), GateAction::Open { .. }));
        // 1.0 s pause: below the close threshold, still fed.
        for _ in 0..20 {
            assert_eq!(gate.push(&quiet(0.05)), GateAction::Feed);
        }
        assert_eq!(gate.push(&loud(0.05)), GateAction::Feed);
        let mut actions = Vec::new();
        for _ in 0..40 {
            actions.push(gate.push(&quiet(0.05)));
        }
        assert_eq!(
            actions
                .iter()
                .filter(|a| **a == GateAction::FeedThenClose)
                .count(),
            1
        );
        assert_eq!(actions.last(), Some(&GateAction::Skip));
    }

    #[test]
    fn jumping_ahead_places_the_next_stream_at_the_new_position() {
        let mut gate = SilenceGate::new(SR);
        gate.push(&loud(0.05));
        gate.set_position(2_050);
        assert!(!gate.is_open());
        match gate.push(&loud(0.05)) {
            GateAction::Open { origin_seconds, .. } => {
                assert!((origin_seconds - 2.05).abs() < 1e-9, "{origin_seconds}")
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn segment_bounds_pad_tokens_and_place_them_on_the_recording() {
        let (start, end) = segment_bounds(10.0, 9.04, 14.08);
        assert!((start - (10.0 + 9.04 - START_PAD_SECONDS)).abs() < 1e-9);
        assert!((end - (10.0 + 14.08 + END_PAD_SECONDS)).abs() < 1e-9);
        let (start, end) = segment_bounds(0.0, 0.05, 0.05);
        assert!(start >= 0.0 && end > start);
    }

    fn words(spec: &[(&str, f64)]) -> (Vec<String>, Vec<f64>) {
        spec.iter().map(|(t, s)| (t.to_string(), *s)).unzip()
    }

    #[test]
    fn pending_tokens_commit_after_trailing_silence() {
        let (tokens, times) = words(&[(" Hello", 1.0), (" there", 1.4), (".", 1.5)]);
        assert_eq!(next_commit(&tokens, &times, 0, 3.0), None);
        assert_eq!(next_commit(&tokens, &times, 0, 3.4), Some(3));
        assert_eq!(next_commit(&tokens, &times, 3, 5.0), None);
        assert_eq!(join_tokens(&tokens[0..3]), "Hello there.");
    }

    #[test]
    fn a_monologue_is_split_at_its_widest_word_gap() {
        // Words every 0.3 s for 24 s with one 0.6 s gap before " Next" at 12 s.
        let mut spec: Vec<(String, f64)> = Vec::new();
        let mut t = 0.0;
        while t < 24.0 {
            let word = if (t - 12.0f64).abs() < 1e-9 {
                " Next"
            } else {
                " word"
            };
            spec.push((word.to_string(), t));
            t = ((t + if (t - 11.4f64).abs() < 1e-9 { 0.6 } else { 0.3 }) * 10.0f64).round() / 10.0;
        }
        let (tokens, times): (Vec<String>, Vec<f64>) = spec.into_iter().unzip();
        let cut = next_commit(&tokens, &times, 0, 24.0).expect("split");
        assert_eq!(tokens[cut], " Next");
        assert!((times[cut] - 12.0).abs() < 1e-9);
    }

    #[test]
    fn splits_never_land_inside_a_word() {
        let mut tokens = Vec::new();
        let mut times = Vec::new();
        for i in 0..80 {
            // " sq" + "ual" + "id" pieces; the only word starts are the " sq" ones.
            let piece = [" sq", "ual", "id"][i % 3];
            tokens.push(piece.to_string());
            times.push(i as f64 * 0.3 + if i % 3 == 1 { 0.2 } else { 0.0 });
        }
        if let Some(cut) = next_commit(&tokens, &times, 0, times[79] + 0.1) {
            assert!(tokens[cut].starts_with(' '));
        }
    }
}
