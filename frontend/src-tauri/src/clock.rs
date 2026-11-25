//! The clock recording-time features schedule by: the live summary's cadence,
//! Copilot's cooldowns, rate caps and timeouts, and the built-in model's
//! background pacing.
//!
//! In the app it is the real clock (`tokio::time`), so behaviour is unchanged.
//! The replay harness (`crate::replay`) swaps in a simulated clock that follows
//! the recording's audio time, so a meeting can be replayed as fast as the
//! machine decodes it while every time-based rule still sees meeting time. It
//! advances at the real rate while model work is in flight ([`busy`]), so a
//! request that takes 1.2 s also lets 1.2 s of audio arrive meanwhile.
//!
//! Latency measurements (first word, tokens/s) stay on the real clock.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

struct Simulated {
    /// Simulated time zero.
    base: Instant,
    /// Simulated time at `following_since` (or now, when not following).
    anchor: Duration,
    /// Since when simulated time advances with the wall clock.
    following_since: Option<std::time::Instant>,
}

impl Simulated {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            anchor: Duration::ZERO,
            following_since: None,
        }
    }

    fn elapsed(&self) -> Duration {
        self.anchor
            + self
                .following_since
                .map(|w| w.elapsed())
                .unwrap_or_default()
    }

    fn advance_to(&mut self, to: Duration) {
        self.anchor = self.elapsed().max(to);
        self.following_since = None;
    }

    fn follow_wall(&mut self, on: bool) {
        self.anchor = self.elapsed();
        self.following_since = on.then(std::time::Instant::now);
    }
}

static SIMULATED_ON: AtomicBool = AtomicBool::new(false);
static SIMULATED: Mutex<Option<Simulated>> = Mutex::new(None);
static BUSY: AtomicUsize = AtomicUsize::new(0);

/// How often a simulated sleep checks the time.
const SIMULATED_POLL: Duration = Duration::from_millis(2);

fn simulated<T>(f: impl FnOnce(&mut Simulated) -> T) -> Option<T> {
    if !SIMULATED_ON.load(Ordering::Acquire) {
        return None;
    }
    SIMULATED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .map(f)
}

/// The current time.
pub fn now() -> Instant {
    simulated(|s| s.base + s.elapsed()).unwrap_or_else(Instant::now)
}

/// Sleeps for `duration` of clock time.
pub async fn sleep(duration: Duration) {
    if SIMULATED_ON.load(Ordering::Acquire) {
        sleep_until(now() + duration).await;
    } else {
        tokio::time::sleep(duration).await;
    }
}

/// Sleeps until `deadline` on this clock.
pub async fn sleep_until(deadline: Instant) {
    if !SIMULATED_ON.load(Ordering::Acquire) {
        tokio::time::sleep_until(deadline).await;
        return;
    }
    while now() < deadline {
        tokio::time::sleep(SIMULATED_POLL).await;
    }
}

/// Marks model work in flight until dropped.
pub struct Busy(());

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Model work (a generation, queued or running) is in flight while the guard
/// lives. Free in the app; the replay paces audio to the wall clock meanwhile.
pub fn busy() -> Busy {
    BUSY.fetch_add(1, Ordering::AcqRel);
    Busy(())
}

pub fn is_busy() -> bool {
    BUSY.load(Ordering::Acquire) > 0
}

/// Control of the simulated clock, for the replay harness.
pub mod simulation {
    use super::*;

    /// Switches every clock reader to simulated time, starting at zero.
    pub fn install() {
        *SIMULATED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Simulated::new());
        SIMULATED_ON.store(true, Ordering::Release);
    }

    pub fn uninstall() {
        SIMULATED_ON.store(false, Ordering::Release);
        *SIMULATED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn is_installed() -> bool {
        SIMULATED_ON.load(Ordering::Acquire)
    }

    /// Simulated time since `install`.
    pub fn elapsed() -> Duration {
        simulated(|s| s.elapsed()).unwrap_or_default()
    }

    /// Jumps forward to `to` (never backwards) and stops following the wall.
    pub fn advance_to(to: Duration) {
        simulated(|s| s.advance_to(to));
    }

    /// Lets simulated time run at the wall clock's rate from now on (`true`),
    /// or freezes it where it is (`false`).
    pub fn follow_wall(on: bool) {
        simulated(|s| s.follow_wall(on));
    }

    pub fn is_following() -> bool {
        simulated(|s| s.following_since.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests use a local `Simulated`: installing the global one would change
    // the clock under other tests running in parallel.
    #[test]
    fn simulated_time_jumps_freezes_and_follows_the_wall() {
        let mut s = Simulated::new();
        s.advance_to(Duration::from_secs(90));
        assert_eq!(s.elapsed(), Duration::from_secs(90));
        s.advance_to(Duration::from_secs(10));
        assert_eq!(s.elapsed(), Duration::from_secs(90), "never backwards");

        s.follow_wall(true);
        std::thread::sleep(Duration::from_millis(30));
        assert!(s.elapsed() >= Duration::from_millis(90_030));
        s.follow_wall(false);
        let frozen = s.elapsed();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(s.elapsed(), frozen);
        s.advance_to(Duration::from_secs(200));
        assert_eq!(s.elapsed(), Duration::from_secs(200));
    }

    #[tokio::test]
    async fn the_real_clock_is_the_default() {
        assert!(!simulation::is_installed());
        let start = now();
        sleep(Duration::from_millis(15)).await;
        assert!(start.elapsed() >= Duration::from_millis(15));
        let _busy = busy();
        assert!(is_busy());
    }
}
