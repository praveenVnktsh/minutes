//! Smoothed download-rate estimation shared by every model download.
//!
//! Download loops report a cumulative byte count every few hundred milliseconds.
//! The instantaneous rate between two such reports swings wildly — a stalled
//! chunk or a burst out of a socket buffer moves it by an order of magnitude —
//! so an ETA computed from it is unreadable. [`DownloadEta`] smooths the rate
//! with an exponentially weighted moving average and refuses to answer at all
//! until the average has had time to settle.

use std::time::{Duration, Instant};

/// Observations required before the rate is reported at all. Three cumulative
/// byte counts give two intervals, enough for the average to have moved off its
/// first, unsmoothed sample.
const MIN_OBSERVATIONS: usize = 3;

/// Elapsed time required between the first and latest observation. Downloads
/// routinely spend their first second on TLS setup and redirects, so a window
/// shorter than this mostly measures connection setup, not throughput.
const MIN_WINDOW: Duration = Duration::from_secs(2);

/// Time constant of the moving average. A rate change is roughly 63% absorbed
/// after this long and essentially complete after three times it, which tracks
/// a genuine bandwidth change within a few seconds without chasing per-chunk
/// jitter.
const SMOOTHING_TAU_SECS: f64 = 5.0;

/// Tracks download throughput over successive cumulative byte counts and turns
/// it into a time remaining.
///
/// Feed it [`observe`](Self::observe) at every progress report. Both
/// [`bytes_per_second`](Self::bytes_per_second) and
/// [`seconds_remaining`](Self::seconds_remaining) return `None` while the
/// estimate cannot be trusted: fewer than three observations, less than two
/// seconds of observation, or a smoothed rate that is not positive and finite.
/// `seconds_remaining` additionally returns `None` for an unknown total
/// (`total_bytes == 0`) and for a download that has already finished.
///
/// Observations that go backwards — an earlier instant, or a byte count that
/// did not advance — are ignored rather than folded in, so a retried or
/// out-of-order report cannot corrupt the average.
#[derive(Debug, Clone, Default)]
pub struct DownloadEta {
    first: Option<(u64, Instant)>,
    last: Option<(u64, Instant)>,
    observations: usize,
    smoothed_rate: Option<f64>,
}

impl DownloadEta {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cumulative downloaded-byte count observed at `at`.
    pub fn observe(&mut self, downloaded_bytes: u64, at: Instant) {
        let Some((last_bytes, last_at)) = self.last else {
            self.first = Some((downloaded_bytes, at));
            self.last = Some((downloaded_bytes, at));
            self.observations = 1;
            return;
        };

        // `checked_duration_since` rather than subtraction: a later-minus-earlier
        // inversion panics, and progress reports can arrive out of order.
        let Some(elapsed) = at.checked_duration_since(last_at) else {
            return;
        };
        let elapsed_secs = elapsed.as_secs_f64();
        if elapsed_secs <= 0.0 {
            return;
        }

        let Some(delta_bytes) = downloaded_bytes.checked_sub(last_bytes) else {
            return;
        };
        if delta_bytes == 0 {
            return;
        }

        let sample_rate = delta_bytes as f64 / elapsed_secs;
        if !sample_rate.is_finite() {
            return;
        }

        self.smoothed_rate = Some(match self.smoothed_rate {
            None => sample_rate,
            Some(previous) => {
                // Time-aware weighting, so irregular report intervals carry the
                // weight they deserve instead of one fixed step each.
                let alpha = (1.0 - (-elapsed_secs / SMOOTHING_TAU_SECS).exp()).clamp(0.0, 1.0);
                previous + alpha * (sample_rate - previous)
            }
        });
        self.last = Some((downloaded_bytes, at));
        self.observations += 1;
    }

    /// Smoothed rate in bytes per second, or None while the rate is unstable.
    pub fn bytes_per_second(&self) -> Option<f64> {
        if self.observations < MIN_OBSERVATIONS {
            return None;
        }

        let (_, first_at) = self.first?;
        let (_, last_at) = self.last?;
        if last_at.checked_duration_since(first_at)? < MIN_WINDOW {
            return None;
        }

        self.smoothed_rate
            .filter(|rate| rate.is_finite() && *rate > 0.0)
    }

    /// Whole seconds remaining, or None while the estimate is indeterminate.
    pub fn seconds_remaining(&self, downloaded_bytes: u64, total_bytes: u64) -> Option<u64> {
        if total_bytes == 0 || downloaded_bytes >= total_bytes {
            return None;
        }

        let rate = self.bytes_per_second()?;
        let remaining = (total_bytes - downloaded_bytes) as f64 / rate;
        if !remaining.is_finite() || remaining < 0.0 {
            return None;
        }

        // `as u64` saturates rather than wrapping, and the guard above rules out
        // NaN, so the ceiling is safe for any finite rate.
        Some(remaining.ceil() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    /// Feeds `rate_bytes_per_sec` for `steps` half-second intervals, returning
    /// the cumulative byte count and instant reached.
    fn feed(
        eta: &mut DownloadEta,
        start_bytes: u64,
        start: Instant,
        rate_bytes_per_sec: u64,
        steps: u32,
    ) -> (u64, Instant) {
        let mut bytes = start_bytes;
        let mut at = start;
        for _ in 0..steps {
            bytes += rate_bytes_per_sec / 2;
            at += Duration::from_millis(500);
            eta.observe(bytes, at);
        }
        (bytes, at)
    }

    #[test]
    fn steady_rate_converges_on_the_right_answer() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        let (downloaded, _) = feed(&mut eta, 0, start, MIB, 40);

        let rate = eta.bytes_per_second().expect("rate after a steady 20s");
        assert!(
            (rate - MIB as f64).abs() < MIB as f64 * 0.05,
            "expected ~1 MiB/s, got {rate}"
        );

        // Ten more seconds of payload left at one MiB per second.
        let total = downloaded + 10 * MIB;
        let remaining = eta
            .seconds_remaining(downloaded, total)
            .expect("eta after a steady 20s");
        assert!(
            (9..=11).contains(&remaining),
            "expected ~10s, got {remaining}"
        );
    }

    #[test]
    fn none_before_enough_samples() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        eta.observe(MIB, start + Duration::from_secs(3));

        assert_eq!(eta.bytes_per_second(), None);
        assert_eq!(eta.seconds_remaining(MIB, 100 * MIB), None);
    }

    #[test]
    fn none_while_the_observation_window_is_too_short() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        // Four observations, but all inside a single second.
        for step in 1..=3 {
            eta.observe(step * MIB, start + Duration::from_millis(step * 250));
        }

        assert_eq!(eta.bytes_per_second(), None);
        assert_eq!(eta.seconds_remaining(3 * MIB, 100 * MIB), None);
    }

    #[test]
    fn none_for_a_zero_total() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        let (downloaded, _) = feed(&mut eta, 0, start, MIB, 10);

        assert!(eta.bytes_per_second().is_some());
        assert_eq!(eta.seconds_remaining(downloaded, 0), None);
    }

    #[test]
    fn none_at_completion() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        let (downloaded, _) = feed(&mut eta, 0, start, MIB, 10);

        assert_eq!(eta.seconds_remaining(downloaded, downloaded), None);
        assert_eq!(eta.seconds_remaining(downloaded + MIB, downloaded), None);
    }

    #[test]
    fn a_halved_rate_is_followed_not_snapped_to() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        let (bytes, at) = feed(&mut eta, 0, start, MIB, 40);
        let before = eta.bytes_per_second().expect("rate before the slowdown");

        let (bytes, at) = feed(&mut eta, bytes, at, MIB / 2, 2);
        let just_after = eta
            .bytes_per_second()
            .expect("rate just after the slowdown");
        assert!(
            just_after > MIB as f64 * 0.6 && just_after < before,
            "expected a partial move from {before} toward 0.5 MiB/s, got {just_after}"
        );

        let (_, _) = feed(&mut eta, bytes, at, MIB / 2, 60);
        let settled = eta
            .bytes_per_second()
            .expect("rate once the slowdown settles");
        assert!(
            (settled - MIB as f64 / 2.0).abs() < MIB as f64 * 0.05,
            "expected ~0.5 MiB/s once settled, got {settled}"
        );
    }

    #[test]
    fn out_of_order_and_duplicate_observations_are_ignored() {
        let mut eta = DownloadEta::new();
        let start = Instant::now();
        eta.observe(0, start);
        let (bytes, at) = feed(&mut eta, 0, start, MIB, 20);
        let rate = eta.bytes_per_second().expect("rate before the bad reports");

        eta.observe(bytes - 4 * MIB, at - Duration::from_secs(4)); // earlier instant
        eta.observe(bytes, at); // same instant, same bytes
        eta.observe(bytes, at + Duration::from_secs(1)); // stalled
        eta.observe(bytes - MIB, at + Duration::from_secs(2)); // rewound count

        assert_eq!(eta.bytes_per_second(), Some(rate));
        assert!(eta.seconds_remaining(bytes, bytes + 10 * MIB).is_some());
    }
}
