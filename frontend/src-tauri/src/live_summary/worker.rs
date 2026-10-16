//! The live summary task of one recording session: collects finalized
//! transcript segments, asks the model for entries when a window is due (one
//! request at a time), stores them once the meeting exists, and publishes
//! snapshots. It never touches the recording; the host gives it everything.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use log::{info, warn};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::llm::{FailureKind, GenerateError, Generated, Generator};
use super::parse::{parse_entries, ParsedEntry};
use super::prompt::{self, PreviousEntry};
use super::scheduler::{Segment, Window, WindowScheduler, CADENCE};
use crate::database::repositories::timeline::TimelineEntryRow;

/// How often the worker checks whether a window is due.
pub const TICK: Duration = Duration::from_secs(2);
/// After the recording stops, how long the last request may still run.
pub const STOP_GRACE: Duration = Duration::from_secs(15);
/// Earlier entries the parser checks new ones against for repeats.
const DEDUPE_HISTORY: usize = 10;
/// How soon a window Copilot pre-empted is tried again.
pub const PREEMPTED_RETRY: Duration = Duration::from_secs(10);
/// Longest back-off after repeated failures, and the wait after a provider
/// refuses the request (bad key, quota, rate limit).
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// After the recording stops, how long unsaved entries keep being retried
/// (a busy database, or the meeting row not written yet).
pub const FINAL_PERSIST_WINDOW: Duration = Duration::from_secs(60);
const FINAL_PERSIST_RETRY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineStatus {
    /// Waiting for the next window.
    Listening,
    /// A request is in flight.
    Generating,
    /// The last window failed; the next one retries.
    Failed,
    /// No summary model can be used.
    Unavailable,
    /// The setting is off.
    Disabled,
    /// The session ended.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEntry {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: String,
    pub text: String,
    pub speaker: Option<String>,
    pub provider: String,
    pub model: String,
    pub created_at: String,
}

impl TimelineEntry {
    pub fn row(&self, meeting_id: &str) -> TimelineEntryRow {
        TimelineEntryRow {
            id: self.id.clone(),
            meeting_id: meeting_id.to_string(),
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            kind: self.kind.clone(),
            text: self.text.clone(),
            speaker: self.speaker.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            created_at: self.created_at.clone(),
        }
    }
}

/// `live-timeline-update` payload and `get_live_timeline` result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineSnapshot {
    pub session_id: String,
    pub meeting_id: Option<String>,
    pub status: TimelineStatus,
    pub error: Option<String>,
    pub entries: Vec<TimelineEntry>,
}

/// What the worker needs from the app. The real host reads recording state
/// and settings and writes SQLite; tests use a fake.
#[async_trait]
pub trait WorkerHost: Send + Sync {
    fn is_paused(&self) -> bool;
    /// False once the recording session is gone (a stop path that did not
    /// signal the worker): the worker then finishes on its own.
    fn session_alive(&self) -> bool;
    fn meeting_id(&self) -> Option<String>;
    fn enabled(&self) -> bool;
    async fn generator(&self) -> Result<Arc<dyn Generator>>;
    async fn persist(&self, rows: &[TimelineEntryRow]) -> Result<()>;
    /// Whether the meeting row still exists, to tell a deleted meeting (drop
    /// its entries: they must not come back) from a transient write failure.
    async fn meeting_exists(&self, meeting_id: &str) -> Result<bool>;
    fn publish(&self, snapshot: &TimelineSnapshot);
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    pub cadence: Duration,
    pub tick: Duration,
    pub stop_grace: Duration,
    pub max_backoff: Duration,
    pub final_persist_window: Duration,
    pub final_persist_retry: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            cadence: CADENCE,
            tick: TICK,
            stop_grace: STOP_GRACE,
            max_backoff: MAX_BACKOFF,
            final_persist_window: FINAL_PERSIST_WINDOW,
            final_persist_retry: FINAL_PERSIST_RETRY,
        }
    }
}

/// The wait before the next window after `failures` failures in a row: the
/// cadence after the first, then doubling, up to `max`.
pub fn backoff_delay(cadence: Duration, failures: u32, max: Duration) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    cadence.saturating_mul(1u32 << doublings).min(max)
}

/// The notice for a provider that refused the request.
pub const REFUSED_NOTICE: &str = "The summary provider refused the request (check its API key, quota or rate limit). Live summary will try again in a few minutes.";

type RequestOutcome = (Window, String, String, Result<Generated, GenerateError>);

struct InFlight {
    handle: JoinHandle<RequestOutcome>,
    cancel: CancellationToken,
}

struct Worker {
    host: Arc<dyn WorkerHost>,
    config: WorkerConfig,
    started: Instant,
    scheduler: WindowScheduler,
    snapshot: TimelineSnapshot,
    unsaved: Vec<TimelineEntry>,
    /// Requests that failed in a row, for the back-off.
    failures: u32,
    in_flight: Option<InFlight>,
}

impl Worker {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn set_status(&mut self, status: TimelineStatus, error: Option<String>) {
        if self.snapshot.status == status && self.snapshot.error == error {
            return;
        }
        self.snapshot.status = status;
        self.snapshot.error = error;
        self.host.publish(&self.snapshot);
    }

    fn refresh_meeting_id(&mut self) {
        if self.snapshot.meeting_id.is_none() {
            self.snapshot.meeting_id = self.host.meeting_id();
        }
    }

    /// Store unsaved entries once the meeting exists. Returns whether
    /// nothing is left unsaved. A failed write keeps the entries for the next
    /// try unless the meeting is gone (deleted mid-recording).
    async fn persist_unsaved(&mut self) -> bool {
        self.refresh_meeting_id();
        let Some(meeting_id) = self.snapshot.meeting_id.clone() else {
            return self.unsaved.is_empty();
        };
        if self.unsaved.is_empty() {
            return true;
        }
        let rows: Vec<_> = self.unsaved.iter().map(|e| e.row(&meeting_id)).collect();
        match self.host.persist(&rows).await {
            Ok(()) => {
                self.unsaved.clear();
                true
            }
            Err(error) => {
                if matches!(self.host.meeting_exists(&meeting_id).await, Ok(false)) {
                    warn!(
                        "Live summary: meeting {meeting_id} is gone, dropping {} unsaved entries",
                        rows.len()
                    );
                    self.unsaved.clear();
                    return true;
                }
                warn!(
                    "Live summary: could not store {} entries for {meeting_id}, will retry: {error:#}",
                    rows.len()
                );
                false
            }
        }
    }

    /// Back off after a failed request; the window's text goes back into the
    /// queue when the failure was the request's, not the reply's.
    fn record_failure(&mut self, window: Option<Window>, kind: FailureKind) {
        if kind != FailureKind::Preempted {
            self.failures += 1;
        }
        let delay = match kind {
            FailureKind::Preempted => PREEMPTED_RETRY.min(self.config.cadence),
            FailureKind::Unavailable => self.config.max_backoff,
            FailureKind::Transient => {
                backoff_delay(self.config.cadence, self.failures, self.config.max_backoff)
            }
        };
        let now = self.now();
        self.scheduler.back_off(now, delay);
        if let Some(window) = window {
            self.scheduler.requeue(window);
        }
        if self.failures > 1 {
            info!(
                "Live summary: {} failures in a row, next window in {}s",
                self.failures,
                delay.as_secs()
            );
        }
    }

    async fn start_request(&mut self, window: Window) {
        let generator = match self.host.generator().await {
            Ok(generator) => generator,
            Err(error) => {
                self.scheduler.complete();
                info!("Live summary unavailable: {error:#}");
                // The panel's own notice covers a missing model.
                self.set_status(TimelineStatus::Unavailable, None);
                return;
            }
        };
        if window.dropped_segments > 0 {
            warn!(
                "Live summary fell behind: skipped {} older segments to catch up",
                window.dropped_segments
            );
        }
        let previous: Vec<PreviousEntry> = self
            .snapshot
            .entries
            .iter()
            .map(|e| PreviousEntry {
                start_ms: e.start_ms,
                kind: e.kind.clone(),
                text: e.text.clone(),
            })
            .collect();
        let system = prompt::system_prompt().to_string();
        let user = prompt::user_prompt(&window, &previous);
        perf_debug!(
            "Live summary request: {} words, {}..{} ms",
            window.words(),
            window.start_ms,
            window.end_ms
        );
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let handle = tokio::spawn(async move {
            let provider = generator.provider().to_string();
            let model = generator.model().to_string();
            let result = generator.generate(&system, &user, &token).await;
            (window, provider, model, result)
        });
        self.in_flight = Some(InFlight { handle, cancel });
        self.set_status(TimelineStatus::Generating, None);
    }

    async fn finish_request(
        &mut self,
        outcome: std::result::Result<RequestOutcome, tokio::task::JoinError>,
    ) {
        self.in_flight = None;
        self.scheduler.complete();
        let (window, provider, model, result) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!("Live summary request task failed: {error}");
                self.record_failure(None, FailureKind::Transient);
                self.set_status(
                    TimelineStatus::Failed,
                    Some("The live summary request failed".into()),
                );
                return;
            }
        };
        let raw =
            match result {
                Ok(generated) if generated.truncated => {
                    // Same text, same seed: a retry would be cut off again.
                    warn!("Live summary: skipping a window, the reply hit the output cap");
                    self.record_failure(None, FailureKind::Transient);
                    self.set_status(
                        TimelineStatus::Failed,
                        Some("The summary model's reply was cut off".into()),
                    );
                    return;
                }
                Ok(generated) => generated.text,
                Err(error) => {
                    if error.kind == FailureKind::Preempted {
                        info!("Live summary: Copilot needed the model, retrying this window");
                    } else {
                        warn!("Live summary request failed: {error}");
                    }
                    self.record_failure(Some(window), error.kind);
                    match error.kind {
                        FailureKind::Preempted => self.set_status(TimelineStatus::Listening, None),
                        FailureKind::Unavailable => self
                            .set_status(TimelineStatus::Unavailable, Some(REFUSED_NOTICE.into())),
                        FailureKind::Transient => self
                            .set_status(TimelineStatus::Failed, Some(short_error(&error.message))),
                    }
                    return;
                }
            };
        let history: Vec<String> = self
            .snapshot
            .entries
            .iter()
            .rev()
            .take(DEDUPE_HISTORY)
            .map(|e| e.text.clone())
            .collect();
        match parse_entries(
            &raw,
            window.start_ms,
            window.end_ms,
            window.max_entries,
            &history,
        ) {
            Ok(parsed) => {
                let created_at = chrono::Utc::now().to_rfc3339();
                let new: Vec<TimelineEntry> = parsed
                    .into_iter()
                    .map(|entry: ParsedEntry| TimelineEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        start_ms: entry.start_ms,
                        end_ms: entry.end_ms,
                        kind: entry.kind.as_str().to_string(),
                        text: entry.text,
                        speaker: entry.speaker,
                        provider: provider.clone(),
                        model: model.clone(),
                        created_at: created_at.clone(),
                    })
                    .collect();
                perf_debug!("Live summary: {} new entries", new.len());
                self.failures = 0;
                self.snapshot.entries.extend(new.iter().cloned());
                self.snapshot.entries.sort_by_key(|e| e.start_ms);
                self.unsaved.extend(new);
                self.persist_unsaved().await;
                // Always publish: the entries changed even if the status did not.
                self.snapshot.status = TimelineStatus::Listening;
                self.snapshot.error = None;
                self.host.publish(&self.snapshot);
            }
            Err(error) => {
                // The reply holds meeting content: only its size reaches the
                // release log.
                warn!(
                    "Live summary: skipping a window, {error} ({} chars)",
                    raw.len()
                );
                self.record_failure(None, FailureKind::Transient);
                perf_debug!(
                    "Live summary unreadable reply: {:?}",
                    raw.chars().take(160).collect::<String>()
                );
                self.set_status(
                    TimelineStatus::Failed,
                    Some("The summary model's reply could not be read".into()),
                );
            }
        }
    }

    async fn tick(&mut self) {
        let _ = self.persist_unsaved().await;
        if !self.host.enabled() {
            self.scheduler.clear();
            self.set_status(TimelineStatus::Disabled, None);
            return;
        }
        if self.snapshot.status == TimelineStatus::Disabled {
            self.set_status(TimelineStatus::Listening, None);
        }
        let now = self.now();
        if let Some(window) = self.scheduler.poll(now, self.host.is_paused()) {
            self.start_request(window).await;
        }
    }

    /// Wait for the request in flight, up to `deadline`, then give up on it.
    ///
    /// Giving up cancels only this worker's interest: a built-in request was
    /// sent with `OnAbandon::LetFinish`, so the shared sidecar finishes it in
    /// the background and drops the reply, and the summary (or anything else
    /// using the sidecar) is never interrupted. The task is detached, never
    /// aborted: aborting could cut a sidecar shutdown or exchange in half.
    async fn settle_in_flight(&mut self, deadline: Instant) {
        let Some(InFlight { mut handle, cancel }) = self.in_flight.take() else {
            return;
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, &mut handle).await {
            Ok(outcome) => self.finish_request(outcome).await,
            Err(_) => {
                info!("Live summary: abandoning the last request at stop");
                cancel.cancel();
                drop(handle);
                self.scheduler.complete();
            }
        }
    }

    async fn finish(&mut self, segments: &mut UnboundedReceiver<Segment>, grace: Duration) {
        let deadline = Instant::now() + grace;
        while let Ok(segment) = segments.try_recv() {
            self.scheduler.push(segment);
        }
        self.settle_in_flight(deadline).await;
        // A provider that just refused us is not asked again for the tail.
        let refused = self.snapshot.status == TimelineStatus::Unavailable;
        if self.host.enabled() && !refused && Instant::now() < deadline {
            let now = self.now();
            if let Some(window) = self.scheduler.flush(now) {
                self.start_request(window).await;
                self.settle_in_flight(deadline).await;
            }
        }
        // The recording no longer waits on us: keep trying to store the last
        // entries for a while (a locked database, a meeting row still being
        // written), then give up.
        let persist_deadline = Instant::now() + self.config.final_persist_window;
        while !self.persist_unsaved().await && Instant::now() < persist_deadline {
            tokio::time::sleep(self.config.final_persist_retry).await;
        }
        if !self.unsaved.is_empty() {
            warn!(
                "Live summary: {} entries were never stored",
                self.unsaved.len()
            );
        }
        self.snapshot.status = TimelineStatus::Stopped;
        self.snapshot.error = None;
        self.host.publish(&self.snapshot);
    }
}

fn short_error(message: &str) -> String {
    let message: String = message.chars().take(160).collect();
    format!("The summary model failed: {message}")
}

/// Run one session until `finish` is cancelled, the segment stream ends, or
/// the recording session disappears. Returns the final snapshot.
pub async fn run(
    host: Arc<dyn WorkerHost>,
    session_id: String,
    mut segments: UnboundedReceiver<Segment>,
    finish: CancellationToken,
    config: WorkerConfig,
) -> TimelineSnapshot {
    let enabled = host.enabled();
    let mut worker = Worker {
        snapshot: TimelineSnapshot {
            session_id,
            meeting_id: host.meeting_id(),
            status: if enabled {
                TimelineStatus::Listening
            } else {
                TimelineStatus::Disabled
            },
            error: None,
            entries: Vec::new(),
        },
        host,
        config,
        started: Instant::now(),
        scheduler: WindowScheduler::with_cadence(Duration::ZERO, config.cadence),
        unsaved: Vec::new(),
        failures: 0,
        in_flight: None,
    };
    worker.host.publish(&worker.snapshot);

    let mut ticker = tokio::time::interval(config.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = finish.cancelled() => break,
            outcome = async {
                match worker.in_flight.as_mut() {
                    Some(in_flight) => (&mut in_flight.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                worker.finish_request(outcome).await;
            }
            segment = segments.recv() => match segment {
                Some(segment) => {
                    perf_trace!("Live summary segment: {} chars", segment.text.len());
                    worker.scheduler.push(segment);
                }
                None => break,
            },
            _ = ticker.tick() => {
                if !worker.host.session_alive() {
                    info!("Live summary: recording session ended without a stop signal");
                    break;
                }
                worker.tick().await;
            }
        }
    }
    worker.finish(&mut segments, config.stop_grace).await;
    worker.snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_summary::scheduler::Speaker;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::sync::mpsc::unbounded_channel;

    #[derive(Default)]
    struct FakeHost {
        paused: AtomicBool,
        disabled: AtomicBool,
        gone: AtomicBool,
        meeting_id: Mutex<Option<String>>,
        fail_persist: AtomicBool,
        meeting_gone: AtomicBool,
        persisted: Mutex<Vec<TimelineEntryRow>>,
        published: Mutex<Vec<TimelineSnapshot>>,
        generator: Mutex<Option<Arc<dyn Generator>>>,
    }

    #[async_trait]
    impl WorkerHost for FakeHost {
        fn is_paused(&self) -> bool {
            self.paused.load(Ordering::SeqCst)
        }
        fn session_alive(&self) -> bool {
            !self.gone.load(Ordering::SeqCst)
        }
        fn meeting_id(&self) -> Option<String> {
            self.meeting_id.lock().unwrap().clone()
        }
        fn enabled(&self) -> bool {
            !self.disabled.load(Ordering::SeqCst)
        }
        async fn generator(&self) -> Result<Arc<dyn Generator>> {
            self.generator
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("No summary model is configured"))
        }
        async fn persist(&self, rows: &[TimelineEntryRow]) -> Result<()> {
            if self.fail_persist.load(Ordering::SeqCst) {
                anyhow::bail!("FOREIGN KEY constraint failed");
            }
            self.persisted.lock().unwrap().extend(rows.iter().cloned());
            Ok(())
        }
        async fn meeting_exists(&self, _: &str) -> Result<bool> {
            Ok(!self.meeting_gone.load(Ordering::SeqCst))
        }
        fn publish(&self, snapshot: &TimelineSnapshot) {
            self.published.lock().unwrap().push(snapshot.clone());
        }
    }

    /// Replies with one entry per call, after `delay`; can fail the first calls
    /// or hang until cancelled.
    struct FakeGenerator {
        delay: Duration,
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
        fail_first: usize,
        fail_kind: FailureKind,
        hang: bool,
        prompts: Mutex<Vec<String>>,
    }

    impl FakeGenerator {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                fail_first: 0,
                fail_kind: FailureKind::Transient,
                hang: false,
                prompts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Generator for FakeGenerator {
        fn provider(&self) -> &str {
            "builtin-ai"
        }
        fn model(&self) -> &str {
            "fake"
        }
        async fn generate(
            &self,
            _: &str,
            user: &str,
            cancel: &CancellationToken,
        ) -> std::result::Result<Generated, GenerateError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompts.lock().unwrap().push(user.to_string());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            let result = if self.hang {
                cancel.cancelled().await;
                Err(GenerateError::transient("cancelled"))
            } else {
                tokio::time::sleep(self.delay).await;
                if call < self.fail_first {
                    Err(GenerateError {
                        kind: self.fail_kind,
                        message: "model offline".into(),
                    })
                } else {
                    // Distinct text per call, so the dedupe keeps every entry.
                    let topic = [
                        "budget", "hiring", "launch", "pricing", "roadmap", "support",
                    ][call % 6];
                    Ok(Generated {
                        text: format!(
                            r#"{{"entries":[{{"kind":"decision","text":"Agreed on the {topic} plan, call {call}","speaker":"Others"}}]}}"#
                        ),
                        truncated: false,
                    })
                }
            };
            assert!(user.contains("New transcript"));
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }

    fn config() -> WorkerConfig {
        WorkerConfig {
            cadence: Duration::from_millis(40),
            tick: Duration::from_millis(10),
            stop_grace: Duration::from_millis(300),
            max_backoff: Duration::from_millis(40),
            final_persist_window: Duration::from_millis(100),
            final_persist_retry: Duration::from_millis(10),
        }
    }

    fn speech(start_s: i64) -> Segment {
        Segment {
            start_ms: start_s * 1000,
            end_ms: start_s * 1000 + 5000,
            speaker: Speaker::Others,
            text: "we went through the plan in detail and everyone agreed that the launch moves to the second week of next month after the design review on Tuesday".into(),
            received_at: Duration::ZERO,
        }
    }

    fn host_with(generator: Arc<FakeGenerator>) -> Arc<FakeHost> {
        let host = Arc::new(FakeHost::default());
        *host.generator.lock().unwrap() = Some(generator);
        *host.meeting_id.lock().unwrap() = Some("meeting-1".into());
        host
    }

    #[tokio::test]
    async fn keeps_one_request_in_flight_while_speech_keeps_coming() {
        let generator = Arc::new(FakeGenerator::new(Duration::from_millis(120)));
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));

        for i in 0..30 {
            tx.send(speech(i * 5)).unwrap();
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        finish.cancel();
        let snapshot = task.await.unwrap();

        assert_eq!(generator.max_active.load(Ordering::SeqCst), 1);
        let calls = generator.calls.load(Ordering::SeqCst);
        assert!(calls >= 2, "expected several windows, got {calls}");
        // 450 ms of speech at 120 ms per request: windows had to merge.
        assert!(calls < 30 / 2);
        assert_eq!(snapshot.status, TimelineStatus::Stopped);
        assert_eq!(snapshot.entries.len(), host.persisted.lock().unwrap().len());
        assert!(snapshot.entries.iter().all(|e| e.kind == "decision"));
    }

    #[tokio::test]
    async fn silence_makes_no_requests() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        let (_tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host, "s".into(), rx, finish.clone(), config()));
        tokio::time::sleep(Duration::from_millis(150)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();
        assert_eq!(generator.calls.load(Ordering::SeqCst), 0);
        assert!(snapshot.entries.is_empty());
    }

    #[tokio::test]
    async fn stopping_flushes_the_tail_window() {
        let generator = Arc::new(FakeGenerator::new(Duration::from_millis(10)));
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let mut config = config();
        config.cadence = Duration::from_secs(600);
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config));
        tx.send(speech(0)).unwrap();
        finish.cancel();
        let snapshot = task.await.unwrap();
        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(host.persisted.lock().unwrap()[0].meeting_id, "meeting-1");
    }

    #[tokio::test]
    async fn a_hung_request_is_cancelled_within_the_stop_grace() {
        let mut generator = FakeGenerator::new(Duration::ZERO);
        generator.hang = true;
        let generator = Arc::new(generator);
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host, "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(generator.calls.load(Ordering::SeqCst), 1);

        let stopped_at = Instant::now();
        finish.cancel();
        let snapshot = task.await.unwrap();
        assert!(stopped_at.elapsed() < Duration::from_millis(1500));
        assert_eq!(snapshot.status, TimelineStatus::Stopped);
        assert_eq!(
            generator.active.load(Ordering::SeqCst),
            0,
            "cancellation reached the request"
        );
    }

    #[tokio::test]
    async fn a_failed_window_reports_failure_and_the_next_one_recovers() {
        let mut generator = FakeGenerator::new(Duration::from_millis(5));
        generator.fail_first = 1;
        let generator = Arc::new(generator);
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(speech(60)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();

        let statuses: Vec<_> = host
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.status)
            .collect();
        let failed_at = statuses
            .iter()
            .position(|s| *s == TimelineStatus::Failed)
            .unwrap();
        assert!(statuses[failed_at..].contains(&TimelineStatus::Listening));
        // The failed window's text is retried (alone or merged with the next
        // speech, depending on timing), so the timeline is not empty.
        assert!(!snapshot.entries.is_empty());
    }

    #[tokio::test]
    async fn entries_wait_for_the_meeting_and_are_dropped_if_it_is_gone() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        *host.meeting_id.lock().unwrap() = None;
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(host.persisted.lock().unwrap().is_empty());

        *host.meeting_id.lock().unwrap() = Some("meeting-late".into());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(host.persisted.lock().unwrap()[0].meeting_id, "meeting-late");

        host.fail_persist.store(true, Ordering::SeqCst);
        host.meeting_gone.store(true, Ordering::SeqCst);
        tx.send(speech(60)).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();
        assert_eq!(snapshot.entries.len(), 2, "the live view keeps both");
        assert_eq!(host.persisted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_busy_database_does_not_lose_entries() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        host.fail_persist.store(true, Ordering::SeqCst);
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        // Far more than the old three attempts (6 s at the real tick).
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(host.persisted.lock().unwrap().is_empty());
        host.fail_persist.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(host.persisted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_window_preempted_by_copilot_is_retried_quietly() {
        let mut generator = FakeGenerator::new(Duration::ZERO);
        generator.fail_first = 1;
        generator.fail_kind = FailureKind::Preempted;
        let generator = Arc::new(generator);
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        let mut first = speech(0);
        first.text = format!("{} budget review", first.text);
        tx.send(first).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();
        let prompts = generator.prompts.lock().unwrap();
        assert!(prompts.len() >= 2, "the pre-empted window is retried");
        assert!(prompts[1].contains("budget review"), "with its text");
        assert!(
            !host
                .published
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.status == TimelineStatus::Failed),
            "a pre-emption is not shown as a failure"
        );
        assert_eq!(snapshot.entries.len(), 1);
    }

    #[tokio::test]
    async fn a_failed_request_puts_its_text_back_for_the_next_window() {
        let mut generator = FakeGenerator::new(Duration::ZERO);
        generator.fail_first = 1;
        let generator = Arc::new(generator);
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        let mut first = speech(0);
        first.text = format!("{} budget review", first.text);
        tx.send(first).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        tx.send(speech(60)).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        finish.cancel();
        task.await.unwrap();
        let prompts = generator.prompts.lock().unwrap();
        assert!(prompts.len() >= 2);
        assert!(
            prompts[1].contains("budget review"),
            "the retry carries the failed window's text"
        );
    }

    #[tokio::test]
    async fn a_refusing_provider_is_unavailable_and_backed_off() {
        let mut generator = FakeGenerator::new(Duration::ZERO);
        generator.fail_first = usize::MAX;
        generator.fail_kind = FailureKind::Unavailable;
        let generator = Arc::new(generator);
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let mut config = config();
        config.max_backoff = Duration::from_secs(60);
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config));
        for i in 0..10 {
            tx.send(speech(i * 60)).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        finish.cancel();
        task.await.unwrap();
        assert_eq!(
            generator.calls.load(Ordering::SeqCst),
            1,
            "no retry every cadence after a refusal"
        );
        let published = host.published.lock().unwrap();
        assert!(published
            .iter()
            .any(|s| s.status == TimelineStatus::Unavailable
                && s.error.as_deref() == Some(REFUSED_NOTICE)));
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let cadence = Duration::from_secs(60);
        let max = Duration::from_secs(600);
        assert_eq!(backoff_delay(cadence, 1, max), Duration::from_secs(60));
        assert_eq!(backoff_delay(cadence, 2, max), Duration::from_secs(120));
        assert_eq!(backoff_delay(cadence, 3, max), Duration::from_secs(240));
        assert_eq!(backoff_delay(cadence, 5, max), max);
        assert_eq!(backoff_delay(cadence, 400, max), max);
    }

    #[tokio::test]
    async fn pausing_or_disabling_stops_requests() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        host.paused.store(true, Ordering::SeqCst);
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let mut config = config();
        config.stop_grace = Duration::from_millis(50);
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(generator.calls.load(Ordering::SeqCst), 0);

        host.paused.store(false, Ordering::SeqCst);
        host.disabled.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        finish.cancel();
        task.await.unwrap();
        assert_eq!(generator.calls.load(Ordering::SeqCst), 0);
        assert!(host
            .published
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.status == TimelineStatus::Disabled));
    }

    #[tokio::test]
    async fn no_model_is_reported_as_unavailable() {
        let host = Arc::new(FakeHost::default());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        finish.cancel();
        task.await.unwrap();
        assert!(host
            .published
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.status == TimelineStatus::Unavailable));
    }

    #[tokio::test]
    async fn the_worker_ends_when_the_session_disappears() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator);
        let (_tx, rx) = unbounded_channel();
        let task = tokio::spawn(run(
            host.clone(),
            "s".into(),
            rx,
            CancellationToken::new(),
            config(),
        ));
        host.gone.store(true, Ordering::SeqCst);
        let snapshot = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.status, TimelineStatus::Stopped);
    }
}
