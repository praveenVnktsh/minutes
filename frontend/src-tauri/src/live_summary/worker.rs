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

use super::enhancer::{EnhancedSnapshot, Sections, UnsavedSection};
use super::facts::Figures;
use super::finetuned::{self, EntryChange, Session, WindowText};
use super::glossary::Glossary;
use super::llm::{FailureKind, GenerateError, Generated, Generator, REQUEST_TIMEOUT};
use super::notes::{decide_topic, NoteContext, MIN_TOPIC_MS};
use super::parse::{parse_notes, parse_topic_decision, ParsedEntry, TopicDecision};
use super::prompt::{self, PreviousEntry, PromptContext};
use super::scheduler::{Scheduler, Segment, Window, WindowScheduler, CADENCE};
use crate::database::repositories::enhanced_sections::EnhancedSectionRow;
use crate::database::repositories::timeline::TimelineEntryRow;
use crate::summary::live_outline::{
    topic_runs, transcript_line, LiveNote, TopicRun, TranscriptLine, UserNote, WrittenSection,
};

/// How often the worker checks whether a window is due.
pub const TICK: Duration = Duration::from_secs(2);
/// After the recording stops, how long the last request may still run.
pub const STOP_GRACE: Duration = Duration::from_secs(15);
/// Earlier entries the parser checks new ones against for repeats.
const DEDUPE_HISTORY: usize = 8;
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

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
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
    /// The section the note sits under. Absent on notes made before topics
    /// existed, so readers must treat it as optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// A sub-bullet's note: the entry it details, or the user's line (its
    /// notes document block id). Only the fine-tuned live notes model writes
    /// sub-bullets; not stored with the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
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
            topic: self.topic.clone(),
        }
    }
}

/// What the next prompt and parse know from the notes so far: the current
/// topic, earlier topics (most recent first), earlier notes, and the names.
pub struct NotesSoFar {
    pub current_topic: Option<String>,
    pub topics: Vec<String>,
    pub previous: Vec<PreviousEntry>,
    pub glossary: Glossary,
}

impl NotesSoFar {
    pub fn of(entries: &[TimelineEntry]) -> Self {
        let mut topics: Vec<String> = Vec::new();
        for topic in entries.iter().rev().filter_map(|e| e.topic.as_ref()) {
            if !topics.contains(topic) {
                topics.push(topic.clone());
            }
        }
        Self {
            current_topic: entries.iter().rev().find_map(|e| e.topic.clone()),
            topics,
            previous: entries
                .iter()
                .map(|e| PreviousEntry {
                    start_ms: e.start_ms,
                    kind: e.kind.clone(),
                    text: e.text.clone(),
                    topic: e.topic.clone(),
                })
                .collect(),
            glossary: Glossary::from_texts(entries.iter().map(|e| e.text.as_str())),
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
    /// The user's own notes so far, with the time each was typed at.
    async fn user_notes(&self) -> Vec<UserNote> {
        Vec::new()
    }
    /// Store an Enhanced notes section written during the meeting.
    async fn persist_section(&self, row: &EnhancedSectionRow) -> Result<()> {
        let _ = row;
        Ok(())
    }
    /// The Enhanced notes as they stand, for the meeting page.
    fn publish_enhanced(&self, snapshot: &EnhancedSnapshot) {
        let _ = snapshot;
    }
    /// The user's notes document line by line, the AI's lines marked, for
    /// the fine-tuned live notes model. `None` when it cannot be read.
    async fn notes_document(&self) -> Option<Vec<finetuned::DocLine>> {
        None
    }
    /// Store entries the fine-tuned model edited.
    async fn update_entries(&self, rows: &[TimelineEntryRow]) -> Result<()> {
        let _ = rows;
        Ok(())
    }
    /// Remove entries whose notes the fine-tuned model or the user dropped.
    async fn delete_entries(&self, meeting_id: &str, ids: &[String]) -> Result<()> {
        let _ = (meeting_id, ids);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    pub cadence: Duration,
    pub tick: Duration,
    pub stop_grace: Duration,
    pub max_backoff: Duration,
    pub final_persist_window: Duration,
    pub final_persist_retry: Duration,
    /// The topic step's own timeout (`TOPIC_STEP_TIMEOUT`).
    pub topic_timeout: Duration,
    /// The topic step and the notes call together (`WINDOW_BUDGET`).
    pub window_budget: Duration,
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
            topic_timeout: TOPIC_STEP_TIMEOUT,
            window_budget: WINDOW_BUDGET,
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

/// The window, provider, model, the notes reply, and the topic the topic
/// step decided for the window (if it answered).
type RequestOutcome = (
    Window,
    String,
    String,
    Result<Generated, GenerateError>,
    Option<String>,
);

/// The topic step answers within this, or the window fails and is retried
/// with the usual back-off: a model that cannot answer a one-line question
/// in time will not write the notes in time either.
pub const TOPIC_STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// One window's requests together (the topic step and the notes) take at
/// most this, so two long timeouts never run back to back.
pub const WINDOW_BUDGET: Duration = REQUEST_TIMEOUT;

/// Providers that get the separate topic step: local models, where a small
/// model asked for notes and a topic together keeps copying the current
/// topic. A cloud model names the topic in its notes call, so each window
/// stays one request there.
pub fn uses_topic_step(provider: &str) -> bool {
    matches!(
        provider,
        "builtin-ai" | "local-llama" | "localllama" | "ollama"
    )
}

/// Whether live notes get the compact prompt (`prompt::compact_system_prompt`):
/// only the built-in live notes model (Gemma 4 E2B), the model it was
/// measured on. Every other model, the built-in summary models included,
/// keeps the JSON prompt (on Qwen 3.5 4B the compact prompt lost precision:
/// 0.72 on the m35 replay, see the 2026-10-17 live notes spec).
pub fn uses_compact_prompt(provider: &str, model: &str) -> bool {
    super::llm::is_builtin(provider)
        && model == crate::summary::summary_engine::models::LIVE_NOTES_MODEL
}

/// The notes call's prompts for `provider` and `model`.
pub fn notes_prompts(
    provider: &str,
    model: &str,
    window: &Window,
    context: &PromptContext<'_>,
) -> (&'static str, String) {
    if uses_compact_prompt(provider, model) {
        (
            prompt::compact_system_prompt(),
            prompt::compact_user_prompt(window, context),
        )
    } else {
        (
            prompt::system_prompt(),
            prompt::user_prompt(window, context),
        )
    }
}

/// The window's topic from a compact reply's topic line: the topic step
/// folded into the notes call, judged as `decide_topic` judges the step's
/// answer (a young topic is kept, a near-duplicate reuses the current title).
/// The meeting's first notes take the proposed title.
pub fn folded_topic(raw: &str, notes: &NotesSoFar, window_start_ms: i64) -> Option<String> {
    let decision = super::parse::compact_topic(raw)?;
    match notes.current_topic.as_deref() {
        Some(current) => decide_topic(
            Some(&decision),
            Some(current),
            prompt::topic_since(&notes.previous, current),
            window_start_ms,
            &notes.topics,
            Some(&notes.glossary),
        ),
        None => match decision {
            TopicDecision::New(title) => super::notes::normalize_topic(&title),
            TopicDecision::Same => None,
        },
    }
}

/// The topic step (`prompt::topic_user_prompt`): the window's topic.
///
/// - `Ok(None)`: no step (a cloud provider, no current topic yet, the tail
///   window at stop) or no usable answer: the notes call names the topic.
/// - `Ok(None)` without a request, too, while the current topic is younger
///   than `MIN_TOPIC_MS`: the step does not end it, but the notes call may
///   still name a clearly different subject (and keeps it otherwise).
/// - `Err`: the step failed (pre-empted, refused, timed out): the whole
///   window fails and goes through the worker's retry and back-off.
///
/// Traced as a `timeline-request` with id `<n>-topic`.
#[allow(clippy::too_many_arguments)]
pub async fn decide_window_topic(
    generator: &dyn Generator,
    window: &Window,
    context: &PromptContext<'_>,
    notes: &NotesSoFar,
    cancel: &CancellationToken,
    timeout: Duration,
    request: u64,
) -> Result<Option<String>, GenerateError> {
    if !uses_topic_step(generator.provider()) {
        return Ok(None);
    }
    let Some(current) = notes.current_topic.as_deref() else {
        return Ok(None);
    };
    let since = prompt::topic_since(&notes.previous, current);
    if since.is_some_and(|since| window.start_ms - since < MIN_TOPIC_MS) {
        return Ok(None);
    }
    let Some(user) = prompt::topic_user_prompt(window, context) else {
        return Ok(None);
    };
    let id = format!("{request}-topic");
    crate::replay::trace::emit(
        "timeline-request",
        || serde_json::json!({ "id": id, "phase": "start", "kind": "topic", "start_ms": window.start_ms, "end_ms": window.end_ms }),
    );
    let step = cancel.child_token();
    let reply = tokio::select! {
        reply = generator.generate(prompt::topic_system_prompt(), &user, &step) => reply,
        _ = crate::clock::sleep(timeout) => {
            step.cancel();
            Err(GenerateError::transient("the topic step timed out"))
        }
    };
    let decided = match &reply {
        Ok(reply) if !reply.truncated => decide_topic(
            parse_topic_decision(&reply.text).as_ref(),
            Some(current),
            since,
            window.start_ms,
            &notes.topics,
            Some(&notes.glossary),
        ),
        _ => None,
    };
    crate::replay::trace::emit("timeline-request", || {
        let outcome = match &reply {
            Ok(reply) if reply.truncated => "truncated".to_string(),
            Ok(_) if decided.is_none() => "unreadable".to_string(),
            Ok(_) => "done".to_string(),
            Err(error) => format!("{:?}", error.kind).to_lowercase(),
        };
        serde_json::json!({
            "id": id,
            "phase": "done",
            "kind": "topic",
            "outcome": outcome,
            "topic": decided,
            "new_topic": decided.as_deref().is_some_and(|d| d != current),
        })
    });
    match reply {
        Ok(_) => Ok(decided),
        Err(error) => {
            log::debug!("Live summary topic step failed: {error}");
            Err(error)
        }
    }
}

struct InFlight {
    handle: JoinHandle<RequestOutcome>,
    cancel: CancellationToken,
}

/// How long a fine-tuned window may take (its one request).
pub const FINETUNED_BUDGET: Duration = Duration::from_secs(30);

/// The run, the section (or why not), and the provider and model.
type SectionOutcome = (
    TopicRun,
    std::result::Result<WrittenSection, GenerateError>,
    String,
    String,
);

struct SectionInFlight {
    handle: JoinHandle<SectionOutcome>,
    cancel: CancellationToken,
}

struct Worker {
    host: Arc<dyn WorkerHost>,
    config: WorkerConfig,
    /// On `crate::clock`: meeting time, which a replay runs faster than real.
    started: tokio::time::Instant,
    scheduler: Scheduler,
    /// Requests started, for the replay trace.
    requests: u64,
    snapshot: TimelineSnapshot,
    unsaved: Vec<TimelineEntry>,
    /// The last summarized window's segments: the next prompt shows its tail
    /// as context.
    carryover: Vec<Segment>,
    /// Requests that failed in a row, for the back-off.
    failures: u32,
    in_flight: Option<InFlight>,
    /// Every final segment so far: the transcript the sections are written from.
    history: Vec<Segment>,
    /// Enhanced notes sections written, queued and waiting to be stored.
    sections: Sections,
    section_in_flight: Option<SectionInFlight>,
    /// The recording stopped: no new sections.
    stopping: bool,
    /// The fine-tuned live notes model's notes, once it is the model in use.
    finetuned: Option<Session>,
    /// The request in flight is a fine-tuned window.
    finetuned_request: bool,
    /// Stored entries the fine-tuned model changed or dropped since, to
    /// write once the meeting exists.
    pending_updates: Vec<TimelineEntry>,
    pending_deletes: Vec<String>,
}

impl Worker {
    fn accept(&mut self, segment: Segment) {
        self.history.push(segment.clone());
        self.scheduler.push(segment);
    }

    fn live_notes(&self) -> Vec<LiveNote> {
        self.snapshot
            .entries
            .iter()
            .map(|e| {
                LiveNote::new(
                    e.start_ms,
                    e.end_ms,
                    &e.kind,
                    &e.text,
                    e.speaker.as_deref(),
                    e.topic.as_deref(),
                )
            })
            .collect()
    }

    fn transcript(&self) -> Vec<TranscriptLine> {
        let mut lines: Vec<TranscriptLine> = self
            .history
            .iter()
            .map(|s| transcript_line(s.start_ms, s.speaker.label(), &s.text))
            .collect();
        lines.sort_by_key(|l| l.ms);
        lines
    }

    fn heard_until(&self) -> i64 {
        self.history.iter().map(|s| s.end_ms).max().unwrap_or(0)
    }

    fn publish_enhanced(&self) {
        let snapshot = self.sections.view(
            &self.snapshot.session_id,
            self.snapshot.meeting_id.clone(),
            &self.live_notes(),
            &self.transcript(),
            self.heard_until(),
        );
        self.host.publish_enhanced(&snapshot);
    }

    /// After new notes: queue the topics that closed, start the next
    /// section if none is being written, and show where the notes stand.
    async fn enhance(&mut self) {
        let runs = topic_runs(&self.live_notes(), self.heard_until());
        self.sections.queue_closed(&runs);
        self.start_section().await;
        self.publish_enhanced();
    }

    async fn start_section(&mut self) {
        if self.section_in_flight.is_some() {
            return;
        }
        let Some(run) = self.sections.queue.pop_front() else {
            return;
        };
        let generator = match self.host.generator().await {
            Ok(generator) => generator,
            Err(error) => {
                info!("Enhanced notes: no model for a section ({error:#})");
                self.sections.failed(run.start_ms);
                return;
            }
        };
        let user_notes = self.host.user_notes().await;
        let transcript = self.transcript();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let handle = tokio::spawn(async move {
            let provider = generator.provider().to_string();
            let model = generator.model().to_string();
            let result = crate::summary::live_enhanced::write_section(
                generator.as_ref(),
                &run,
                &transcript,
                &user_notes,
                &token,
            )
            .await;
            (run, result, provider, model)
        });
        self.section_in_flight = Some(SectionInFlight { handle, cancel });
    }

    async fn finish_section(
        &mut self,
        outcome: std::result::Result<SectionOutcome, tokio::task::JoinError>,
    ) {
        self.section_in_flight = None;
        match outcome {
            Ok((run, Ok(section), provider, model)) => {
                perf_debug!(
                    "Enhanced notes: wrote a section of {} chars",
                    section.body.len()
                );
                self.sections.written(UnsavedSection {
                    section,
                    end_ms: run.end_ms,
                    provider,
                    model,
                });
                self.persist_sections().await;
            }
            Ok((run, Err(error), _, _)) => {
                warn!("Enhanced notes: a section failed ({error}); it is retried later");
                self.sections.failed(run.start_ms);
            }
            Err(error) => warn!("Enhanced notes: the section task failed: {error}"),
        }
        self.start_section().await;
        self.publish_enhanced();
    }

    /// Store written sections once the meeting exists. Returns whether
    /// nothing is left unsaved.
    async fn persist_sections(&mut self) -> bool {
        self.refresh_meeting_id();
        let Some(meeting_id) = self.snapshot.meeting_id.clone() else {
            return self.sections.unsaved.is_empty();
        };
        let mut left = Vec::new();
        for unsaved in std::mem::take(&mut self.sections.unsaved) {
            if let Err(error) = self.host.persist_section(&unsaved.row(&meeting_id)).await {
                if matches!(self.host.meeting_exists(&meeting_id).await, Ok(false)) {
                    continue;
                }
                warn!("Enhanced notes: could not store a section, will retry: {error:#}");
                left.push(unsaved);
            }
        }
        self.sections.unsaved = left;
        self.sections.unsaved.is_empty()
    }

    /// At stop: let the section being written finish, up to `deadline`, and
    /// start no other (the stop writes what is left).
    async fn settle_section(&mut self, deadline: tokio::time::Instant) {
        self.sections.queue.clear();
        let Some(SectionInFlight { mut handle, cancel }) = self.section_in_flight.take() else {
            return;
        };
        tokio::select! {
            outcome = &mut handle => self.finish_section(outcome).await,
            _ = crate::clock::sleep_until(deadline) => {
                info!("Enhanced notes: leaving the section in flight to the stop");
                cancel.cancel();
                drop(handle);
            }
        }
        if let Some(in_flight) = self.section_in_flight.take() {
            in_flight.cancel.cancel();
        }
        self.sections.queue.clear();
    }

    fn now(&self) -> Duration {
        crate::clock::now().saturating_duration_since(self.started)
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
            return self.unsaved.is_empty()
                && self.pending_updates.is_empty()
                && self.pending_deletes.is_empty();
        };
        if self.unsaved.is_empty() {
            return self.persist_edits(&meeting_id).await;
        }
        let rows: Vec<_> = self.unsaved.iter().map(|e| e.row(&meeting_id)).collect();
        match self.host.persist(&rows).await {
            Ok(()) => {
                self.unsaved.clear();
                self.persist_edits(&meeting_id).await
            }
            Err(error) => {
                if matches!(self.host.meeting_exists(&meeting_id).await, Ok(false)) {
                    warn!(
                        "Live summary: meeting {meeting_id} is gone, dropping {} unsaved entries",
                        rows.len()
                    );
                    self.unsaved.clear();
                    self.pending_updates.clear();
                    self.pending_deletes.clear();
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

    /// Store the fine-tuned model's edits and drops of stored entries.
    async fn persist_edits(&mut self, meeting_id: &str) -> bool {
        if !self.pending_updates.is_empty() {
            let rows: Vec<_> = self
                .pending_updates
                .iter()
                .map(|e| e.row(meeting_id))
                .collect();
            if let Err(error) = self.host.update_entries(&rows).await {
                warn!(
                    "Live summary: could not store {} edited entries, will retry: {error:#}",
                    rows.len()
                );
                return false;
            }
            self.pending_updates.clear();
        }
        if !self.pending_deletes.is_empty() {
            if let Err(error) = self
                .host
                .delete_entries(meeting_id, &self.pending_deletes)
                .await
            {
                warn!(
                    "Live summary: could not remove {} entries, will retry: {error:#}",
                    self.pending_deletes.len()
                );
                return false;
            }
            self.pending_deletes.clear();
        }
        true
    }

    /// Switch to the fine-tuned live notes model's format and windows.
    fn use_finetuned(&mut self, segments: Vec<Segment>) {
        info!("Live summary: the fine-tuned live notes model writes the notes");
        self.scheduler.to_pauses(segments);
        self.finetuned = Some(Session::default());
    }

    /// One fine-tuned window: the user's notes so far, then the request.
    async fn start_finetuned(&mut self, generator: Arc<dyn Generator>, window: Window) {
        let document = self.host.notes_document().await;
        let Some(session) = self.finetuned.as_mut() else {
            return;
        };
        let removed = document
            .map(|lines| session.sync_document(&lines))
            .unwrap_or_default();
        let user = session.prompt(&WindowText::of(&window, &self.carryover));
        self.remove_entries(removed);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        self.requests += 1;
        let request = self.requests;
        crate::replay::trace::emit("timeline-request", || {
            serde_json::json!({
                "id": request,
                "phase": "start",
                "kind": "finetuned",
                "start_ms": window.start_ms,
                "end_ms": window.end_ms,
                "words": window.words(),
            })
        });
        let budget = self.config.window_budget.min(FINETUNED_BUDGET);
        let handle = tokio::spawn(async move {
            let provider = generator.provider().to_string();
            let model = generator.model().to_string();
            let (system, user) = finetuned::turns(&user);
            let call = token.child_token();
            let result = tokio::select! {
                result = generator.generate_finetuned(system, &user, &call) => result,
                _ = crate::clock::sleep(budget) => {
                    call.cancel();
                    Err(GenerateError::transient("the window ran out of time"))
                }
            };
            (window, provider, model, result, None)
        });
        self.in_flight = Some(InFlight { handle, cancel });
        self.finetuned_request = true;
        self.set_status(TimelineStatus::Generating, None);
    }

    /// The fine-tuned model's answer: applied to its notes, then to the
    /// entries. An answer cut off or unreadable changes nothing.
    async fn finish_finetuned(&mut self, window: Window, provider: &str, model: &str, raw: &str) {
        let Some(session) = self.finetuned.as_mut() else {
            return;
        };
        // `_applied` only feeds debug builds' log (perf_debug).
        let (changes, _applied) = session.apply_answer(raw, &window, provider, model);
        perf_debug!(
            "Live summary: fine-tuned answer, {} ops ({} rejected), {} entry changes",
            _applied.len(),
            _applied.iter().filter(|a| !a.rejected.is_empty()).count(),
            changes.len()
        );
        self.carryover = window.segments;
        self.failures = 0;
        let changed = !changes.is_empty();
        self.apply_changes(changes);
        self.persist_unsaved().await;
        self.snapshot.status = TimelineStatus::Listening;
        self.snapshot.error = None;
        self.host.publish(&self.snapshot);
        if changed && !self.stopping {
            self.enhance().await;
        }
    }

    fn apply_changes(&mut self, changes: Vec<EntryChange>) {
        for change in changes {
            match change {
                EntryChange::Add(entry) => {
                    self.snapshot.entries.push(entry.clone());
                    self.unsaved.push(entry);
                }
                EntryChange::Update(entry) => {
                    if let Some(shown) = self.snapshot.entries.iter_mut().find(|e| e.id == entry.id)
                    {
                        *shown = entry.clone();
                    }
                    if let Some(unsaved) = self.unsaved.iter_mut().find(|e| e.id == entry.id) {
                        *unsaved = entry;
                    } else {
                        self.pending_updates.retain(|e| e.id != entry.id);
                        self.pending_updates.push(entry);
                    }
                }
                EntryChange::Remove(id) => self.remove_entries(vec![id]),
            }
        }
        self.snapshot.entries.sort_by_key(|e| e.start_ms);
    }

    fn remove_entries(&mut self, ids: Vec<String>) {
        for id in ids {
            self.snapshot.entries.retain(|e| e.id != id);
            self.pending_updates.retain(|e| e.id != id);
            let before = self.unsaved.len();
            self.unsaved.retain(|e| e.id != id);
            if self.unsaved.len() == before {
                self.pending_deletes.push(id);
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

    async fn start_request(&mut self, window: Window, final_window: bool) {
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
        if generator.live_notes_adapter().is_some() {
            if self.finetuned.is_none() {
                // The model became the fine-tuned one mid-recording: this
                // text goes into pause-aligned windows instead.
                self.scheduler.complete();
                self.use_finetuned(window.segments);
                return;
            }
            self.start_finetuned(generator, window).await;
            return;
        }
        self.finetuned_request = false;
        if window.dropped_segments > 0 {
            warn!(
                "Live summary fell behind: skipped {} older segments to catch up",
                window.dropped_segments
            );
        }
        let notes = NotesSoFar::of(&self.snapshot.entries);
        let carryover = self.carryover.clone();
        perf_debug!(
            "Live summary request: {} words, {}..{} ms",
            window.words(),
            window.start_ms,
            window.end_ms
        );
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        self.requests += 1;
        let request = self.requests;
        crate::replay::trace::emit("timeline-request", || {
            serde_json::json!({
                "id": request,
                "phase": "start",
                "start_ms": window.start_ms,
                "end_ms": window.end_ms,
                "words": window.words(),
                "dropped_segments": window.dropped_segments,
            })
        });
        let (budget, topic_timeout) = (self.config.window_budget, self.config.topic_timeout);
        let handle = tokio::spawn(async move {
            let provider = generator.provider().to_string();
            let model = generator.model().to_string();
            let mut window = window;
            if uses_compact_prompt(&provider, &model) {
                window.max_entries += prompt::COMPACT_EXTRA_NOTES;
            }
            let glossary = notes.glossary.terms();
            let context = PromptContext {
                current_topic: notes.current_topic.as_deref(),
                previous: &notes.previous,
                glossary: &glossary,
                carryover: &carryover,
                decided_topic: None,
            };
            let deadline = crate::clock::now() + budget;
            let decided = if final_window {
                // The tail window at stop: one request, within the grace.
                Ok(None)
            } else {
                decide_window_topic(
                    generator.as_ref(),
                    &window,
                    &context,
                    &notes,
                    &token,
                    topic_timeout.min(budget),
                    request,
                )
                .await
            };
            let decided = match decided {
                Ok(decided) => decided,
                Err(error) => return (window, provider, model, Err(error), None),
            };
            let (system, user) = notes_prompts(
                &provider,
                &model,
                &window,
                &PromptContext {
                    decided_topic: decided.as_deref(),
                    ..context
                },
            );
            let notes_call = token.child_token();
            let result = tokio::select! {
                result = generator.generate_notes(system, &user, 1 + window.max_entries, &notes_call) => result,
                _ = crate::clock::sleep_until(deadline) => {
                    notes_call.cancel();
                    Err(GenerateError::transient("the window ran out of time"))
                }
            };
            (window, provider, model, result, decided)
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
        let request = self.requests;
        crate::replay::trace::emit("timeline-request", || {
            let outcome = match &outcome {
                Ok((_, _, _, Ok(generated), _)) if generated.truncated => "truncated".to_string(),
                Ok((_, _, _, Ok(_), _)) => "done".to_string(),
                Ok((_, _, _, Err(error), _)) => format!("{:?}", error.kind).to_lowercase(),
                Err(_) => "panicked".to_string(),
            };
            serde_json::json!({ "id": request, "phase": "done", "outcome": outcome })
        });
        let (window, provider, model, result, decided) = match outcome {
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
        let result = match result {
            // The closed loop reads a cut-off answer too: its complete lines
            // may parse.
            Ok(generated) if self.finetuned_request => {
                self.finish_finetuned(window, &provider, &model, &generated.text)
                    .await;
                return;
            }
            result => result,
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
        let notes = NotesSoFar::of(&self.snapshot.entries);
        // Without a topic step answer, a compact reply's topic line names it.
        let compact = uses_compact_prompt(&provider, &model);
        let decided = match decided {
            None if compact => folded_topic(&raw, &notes, window.start_ms),
            decided => decided,
        };
        // A note's figures must come from what it was written from.
        let figures = Figures::of(&support_text(&self.carryover, &window.segments));
        let context = NoteContext {
            current_topic: notes.current_topic.as_deref(),
            known_topics: &notes.topics,
            glossary: Some(&notes.glossary),
            previous_texts: &history,
            figures: Some(&figures),
            decided_topic: decided.as_deref(),
            drop_fragments: compact,
            segments: &window.segments,
        };
        match parse_notes(
            &raw,
            window.start_ms,
            window.end_ms,
            window.max_entries,
            &context,
        ) {
            Ok(parsed) => {
                self.carryover = window.segments;
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
                        topic: entry.topic,
                        parent: None,
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
                if !self.stopping {
                    self.enhance().await;
                }
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
        if !self.sections.unsaved.is_empty() {
            let _ = self.persist_sections().await;
        }
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
            self.start_request(window, false).await;
        }
    }

    /// Wait for the request in flight, up to `deadline`, then give up on it.
    ///
    /// Giving up cancels only this worker's interest: a built-in request was
    /// sent with `OnAbandon::LetFinish`, so the shared sidecar finishes it in
    /// the background and drops the reply, and the summary (or anything else
    /// using the sidecar) is never interrupted. The task is detached, never
    /// aborted: aborting could cut a sidecar shutdown or exchange in half.
    async fn settle_in_flight(&mut self, deadline: tokio::time::Instant) {
        let Some(InFlight { mut handle, cancel }) = self.in_flight.take() else {
            return;
        };
        let settled = tokio::select! {
            outcome = &mut handle => Some(outcome),
            _ = crate::clock::sleep_until(deadline) => None,
        };
        match settled {
            Some(outcome) => self.finish_request(outcome).await,
            None => {
                info!("Live summary: abandoning the last request at stop");
                cancel.cancel();
                drop(handle);
                self.scheduler.complete();
            }
        }
    }

    async fn finish(&mut self, segments: &mut UnboundedReceiver<Segment>, grace: Duration) {
        let deadline = crate::clock::now() + grace;
        while let Ok(segment) = segments.try_recv() {
            self.accept(segment);
        }
        self.settle_in_flight(deadline).await;
        // A provider that just refused us is not asked again for the tail.
        let refused = self.snapshot.status == TimelineStatus::Unavailable;
        if self.host.enabled() && !refused {
            // Pause-aligned windows may be waiting: each goes out in order
            // while the grace lasts, until one fails.
            while crate::clock::now() < deadline {
                let now = self.now();
                let Some(window) = self.scheduler.flush(now) else {
                    break;
                };
                self.start_request(window, true).await;
                self.settle_in_flight(deadline).await;
                if !self.scheduler.is_pauses() || self.snapshot.status != TimelineStatus::Listening
                {
                    break;
                }
            }
        }
        // A section being written may finish within the grace; the rest, and
        // the topic under way, are written by the summary at stop.
        self.stopping = true;
        self.settle_section(deadline).await;
        // The recording no longer waits on us: keep trying to store the last
        // entries for a while (a locked database, a meeting row still being
        // written), then give up.
        let persist_deadline = Instant::now() + self.config.final_persist_window;
        loop {
            let entries_saved = self.persist_unsaved().await;
            let sections_saved = self.persist_sections().await;
            if (entries_saved && sections_saved) || Instant::now() >= persist_deadline {
                break;
            }
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

/// The transcript a window's notes may draw on: the context lines and the window.
pub fn support_text(carryover: &[Segment], segments: &[Segment]) -> String {
    carryover
        .iter()
        .chain(segments)
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
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
        started: crate::clock::now(),
        scheduler: Scheduler::Timed(WindowScheduler::with_cadence(
            Duration::ZERO,
            config.cadence,
        )),
        requests: 0,
        unsaved: Vec::new(),
        carryover: Vec::new(),
        failures: 0,
        in_flight: None,
        history: Vec::new(),
        sections: Sections::default(),
        section_in_flight: None,
        stopping: false,
        finetuned: None,
        finetuned_request: false,
        pending_updates: Vec::new(),
        pending_deletes: Vec::new(),
    };
    worker.host.publish(&worker.snapshot);
    // The fine-tuned live notes model gets its own windows from the start.
    if enabled
        && worker
            .host
            .generator()
            .await
            .is_ok_and(|generator| generator.live_notes_adapter().is_some())
    {
        worker.use_finetuned(Vec::new());
    }

    // A deadline rather than a fresh sleep per loop, so frequent segments
    // never starve the tick.
    let mut next_tick = crate::clock::now();
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
            outcome = async {
                match worker.section_in_flight.as_mut() {
                    Some(in_flight) => (&mut in_flight.handle).await,
                    None => std::future::pending().await,
                }
            } => {
                worker.finish_section(outcome).await;
            }
            segment = segments.recv() => match segment {
                Some(segment) => {
                    perf_trace!("Live summary segment: {} chars", segment.text.len());
                    worker.accept(segment);
                }
                None => break,
            },
            _ = crate::clock::sleep_until(next_tick) => {
                next_tick = crate::clock::now() + config.tick;
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
        sections: Mutex<Vec<EnhancedSectionRow>>,
        enhanced: Mutex<Vec<EnhancedSnapshot>>,
        document: Mutex<Option<Vec<finetuned::DocLine>>>,
        updated: Mutex<Vec<TimelineEntryRow>>,
        deleted: Mutex<Vec<String>>,
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
        async fn user_notes(&self) -> Vec<UserNote> {
            vec![UserNote {
                ms: Some(30_000),
                text: "ask about the venue".into(),
            }]
        }
        async fn persist_section(&self, row: &EnhancedSectionRow) -> Result<()> {
            self.sections.lock().unwrap().push(row.clone());
            Ok(())
        }
        fn publish_enhanced(&self, snapshot: &EnhancedSnapshot) {
            self.enhanced.lock().unwrap().push(snapshot.clone());
        }
        async fn notes_document(&self) -> Option<Vec<finetuned::DocLine>> {
            self.document.lock().unwrap().clone()
        }
        async fn update_entries(&self, rows: &[TimelineEntryRow]) -> Result<()> {
            self.updated.lock().unwrap().extend(rows.iter().cloned());
            Ok(())
        }
        async fn delete_entries(&self, _: &str, ids: &[String]) -> Result<()> {
            self.deleted.lock().unwrap().extend(ids.iter().cloned());
            Ok(())
        }
    }

    /// The fine-tuned live notes model: answers from a script, in order.
    struct Finetuned {
        answers: Mutex<std::collections::VecDeque<&'static str>>,
        prompts: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Generator for Finetuned {
        fn provider(&self) -> &str {
            "builtin-ai"
        }
        fn model(&self) -> &str {
            crate::summary::summary_engine::models::LIVE_NOTES_MODEL
        }
        async fn generate(
            &self,
            _: &str,
            _: &str,
            _: &CancellationToken,
        ) -> std::result::Result<Generated, GenerateError> {
            panic!("the fine-tuned mode only asks for fine-tuned windows")
        }
        fn live_notes_adapter(&self) -> Option<&std::path::Path> {
            Some(std::path::Path::new("/models/minutes-notes-lora.gguf"))
        }
        async fn generate_finetuned(
            &self,
            system: &str,
            user: &str,
            _: &CancellationToken,
        ) -> std::result::Result<Generated, GenerateError> {
            self.prompts
                .lock()
                .unwrap()
                .push((system.to_string(), user.to_string()));
            let text = self.answers.lock().unwrap().pop_front().unwrap_or(".");
            Ok(Generated {
                text: text.to_string(),
                truncated: false,
            })
        }
    }

    #[tokio::test]
    async fn the_fine_tuned_model_gets_pause_windows_in_its_format_and_edits_its_notes() {
        let generator = Arc::new(Finetuned {
            answers: Mutex::new(
                [
                    "T=Launch plan\n+d 00:10|Launch moves to the second week of May",
                    "~1|Launch moves to May 14\n+p 00:30|Venue booked in Lisbon for the launch event",
                    "-2",
                ]
                .into(),
            ),
            prompts: Mutex::new(Vec::new()),
        });
        let host = Arc::new(FakeHost::default());
        *host.generator.lock().unwrap() = Some(generator.clone());
        *host.meeting_id.lock().unwrap() = Some("meeting-1".into());
        *host.document.lock().unwrap() = Some(vec![finetuned::DocLine {
            block_id: "b1".into(),
            ms: Some(5_000),
            text: "check catering".into(),
            ai_entry: None,
        }]);
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        let said = |start: i64, end: i64, speaker| Segment {
            start_ms: start,
            end_ms: end,
            speaker,
            text: speech(0).text,
            received_at: Duration::ZERO,
        };
        // A change of speaker after 20 s closes a window.
        tx.send(said(0, 21_000, Speaker::You)).unwrap();
        tx.send(said(21_500, 43_000, Speaker::Others)).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        tx.send(said(43_500, 66_000, Speaker::You)).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        // The last window goes out at stop.
        finish.cancel();
        let snapshot = task.await.unwrap();

        let prompts = generator.prompts.lock().unwrap().clone();
        assert_eq!(prompts.len(), 3);
        assert!(prompts
            .iter()
            .all(|(system, _)| system == finetuned::SYSTEM_PROMPT));
        let first = &prompts[0].1;
        assert!(first.starts_with("WINDOW: 00:00 to 00:21   BUDGET: at most 1 new notes\nTOPIC: none yet\nNEXT ID: 1\nCURRENT NOTES:\n  (none)\nCONTEXT: (none)\nNEW TRANSCRIPT:\n[00:00] You: we went"), "{first}");
        let second = &prompts[1].1;
        assert!(
            second.contains("TOPIC: Launch plan (since 00:00)\nNEXT ID: 2"),
            "{second}"
        );
        assert!(second.contains("CURRENT NOTES:\n1 [ai] Launch moves to the second week of May (decision)\nu1 [you] check catering\n"), "{second}");
        assert!(
            second.contains("CONTEXT (already covered, never note it):\n[00:00] You: we went"),
            "{second}"
        );
        assert!(prompts[2]
            .1
            .contains("2 [ai] Venue booked in Lisbon for the launch event"));

        // One entry is left, edited; the dropped one is gone everywhere.
        assert_eq!(snapshot.entries.len(), 1);
        let entry = &snapshot.entries[0];
        assert_eq!(
            (entry.kind.as_str(), entry.text.as_str()),
            ("decision", "Launch moves to May 14")
        );
        assert_eq!(entry.topic.as_deref(), Some("Launch plan"));
        let persisted = host.persisted.lock().unwrap().clone();
        assert_eq!(persisted.len(), 2);
        let updated = host.updated.lock().unwrap().clone();
        assert_eq!(updated.len(), 1);
        assert_eq!(
            (updated[0].id.as_str(), updated[0].text.as_str()),
            (entry.id.as_str(), "Launch moves to May 14")
        );
        assert_eq!(*host.deleted.lock().unwrap(), vec![persisted[1].id.clone()]);
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
        /// The topic step's questions and its answer; not counted as calls.
        topic_prompts: Mutex<Vec<String>>,
        topic_answer: Mutex<String>,
        /// The topic step fails this way (once), or never answers.
        topic_fail: Mutex<Option<GenerateError>>,
        topic_hang: bool,
        provider: &'static str,
        model: &'static str,
        /// The line limit of each notes call.
        max_lines: Mutex<Vec<usize>>,
        /// Enhanced notes section calls.
        section_prompts: Mutex<Vec<String>>,
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
                topic_prompts: Mutex::new(Vec::new()),
                topic_answer: Mutex::new(r#"{"same":true}"#.into()),
                topic_fail: Mutex::new(None),
                topic_hang: false,
                // A built-in summary model: the JSON prompt with the topic
                // step. Only the live notes model gets the compact prompt.
                provider: "builtin-ai",
                model: "qwen3.5:4b",
                max_lines: Mutex::new(Vec::new()),
                section_prompts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Generator for FakeGenerator {
        fn provider(&self) -> &str {
            self.provider
        }
        async fn generate_notes(
            &self,
            system: &str,
            user: &str,
            max_lines: usize,
            cancel: &CancellationToken,
        ) -> std::result::Result<Generated, GenerateError> {
            self.max_lines.lock().unwrap().push(max_lines);
            self.generate(system, user, cancel).await
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn generate(
            &self,
            system: &str,
            user: &str,
            cancel: &CancellationToken,
        ) -> std::result::Result<Generated, GenerateError> {
            if system == crate::summary::live_outline::section_system_prompt() {
                self.section_prompts.lock().unwrap().push(user.to_string());
                return Ok(Generated {
                    text: "- Launch moves to the second week of next month [00:00]".into(),
                    truncated: false,
                });
            }
            if system == prompt::topic_system_prompt() {
                self.topic_prompts.lock().unwrap().push(user.to_string());
                if self.topic_hang {
                    cancel.cancelled().await;
                    return Err(GenerateError::transient("cancelled"));
                }
                if let Some(error) = self.topic_fail.lock().unwrap().take() {
                    return Err(error);
                }
                return Ok(Generated {
                    text: self.topic_answer.lock().unwrap().clone(),
                    truncated: false,
                });
            }
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
                    // Distinct text per call, so the dedupe keeps every entry
                    // (no figures: the transcript has none).
                    let text = [
                        "Budget for Acme Robotics approved by finance",
                        "Hiring freeze lifted for the support team",
                        "Launch slips by a week after design review",
                        "Pricing tiers stay unchanged through the summer",
                        "Roadmap adds offline sync before the beta",
                        "Support hours extend into weekends",
                    ][call % 6];
                    let text = if system == prompt::compact_system_prompt() {
                        let topic = if call == 0 { "launch plan" } else { "same" };
                        format!("topic: {topic}\n00:01|decision|-|{text}\n")
                    } else {
                        format!(
                            r#"{{"entries":[{{"kind":"decision","text":"{text}","speaker":"Others","topic":"launch plan"}}]}}"#
                        )
                    };
                    Ok(Generated {
                        text,
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
            topic_timeout: Duration::from_secs(5),
            window_budget: Duration::from_secs(10),
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
    async fn later_prompts_carry_the_topic_names_and_previous_context() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut second = speech(60);
        second.text = format!("{} and then the budget", second.text);
        tx.send(second).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();

        let prompts = generator.prompts.lock().unwrap();
        assert!(prompts.len() >= 2);
        assert!(!prompts[0].contains("Current topic"));
        // The current topic is a minute old: the topic step is not asked,
        // and the notes call sees the current topic to keep.
        assert!(generator.topic_prompts.lock().unwrap().is_empty());
        assert!(prompts[1].contains("Current topic: Launch Plan (since 00:00)\n"));
        assert!(prompts[1].contains("Glossary (use these spellings): Acme Robotics"));
        assert!(prompts[1].contains("Earlier transcript (context only, already covered):\n[00:00]"));
        assert!(prompts[1].contains(
            "Notes so far (do not repeat):\nTopic \"Launch Plan\":\n- [00:00] (decision)"
        ));
        assert!(snapshot
            .entries
            .iter()
            .all(|e| e.topic.as_deref() == Some("Launch Plan") && e.speaker.is_none()));
        let persisted = host.persisted.lock().unwrap();
        assert_eq!(persisted[0].topic.as_deref(), Some("Launch Plan"));
    }

    #[tokio::test]
    async fn the_built_in_model_gets_the_compact_prompt_and_its_topic_line() {
        let mut fake = FakeGenerator::new(Duration::ZERO);
        fake.model = crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
        let generator = Arc::new(fake);
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

        let prompts = generator.prompts.lock().unwrap();
        assert!(prompts[0].starts_with("No topic yet: name it on the topic line\n"));
        // One note more than the scheduler's two, and the call stops after
        // the topic line and those three notes.
        assert!(prompts[0].contains("then up to 3 notes about the new transcript"));
        assert_eq!(*generator.max_lines.lock().unwrap(), vec![4, 4]);
        assert!(prompts[1].contains("Current topic: Launch Plan (since 00:01)\n"));
        assert!(prompts[1].contains("topic: Launch Plan\n00:01|decision|-|"));
        let topics: Vec<_> = snapshot
            .entries
            .iter()
            .map(|e| e.topic.as_deref())
            .collect();
        assert_eq!(topics, vec![Some("Launch Plan"), Some("Launch Plan")]);
        assert!(snapshot.entries.iter().all(|e| e.kind == "decision"));
    }

    #[tokio::test]
    async fn a_closed_topic_gets_its_enhanced_section_written_and_stored() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        for start in [0, 60, 120] {
            tx.send(speech(start)).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Three notes under Launch Plan, the topic still open: nothing yet.
        assert!(generator.section_prompts.lock().unwrap().is_empty());
        let open = host.enhanced.lock().unwrap().last().cloned().unwrap();
        assert_eq!(open.sections.len(), 1);
        assert!(open.sections[0].writing && open.sections[0].body.is_empty());

        *generator.topic_answer.lock().unwrap() = r#"{"same":false,"topic":"Globex pitch"}"#.into();
        tx.send(speech(240)).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        finish.cancel();
        task.await.unwrap();

        let prompts = generator.section_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "only the closed topic is written");
        assert!(prompts[0].starts_with("Part of the meeting: Launch Plan (00:00–04:00)"));
        assert!(prompts[0].contains("do not repeat):\n- ask about the venue"));
        assert!(prompts[0].contains("[02:00] [Others] we went through the plan"));
        assert!(!prompts[0].contains("[04:00] [Others]"));
        let sections = host.sections.lock().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].meeting_id, "meeting-1");
        assert_eq!(sections[0].title, "Launch Plan");
        assert_eq!(
            sections[0].body,
            "- ask about the venue\n- Launch moves to the second week of next month [00:00]"
        );
        let last = host.enhanced.lock().unwrap().last().cloned().unwrap();
        let shape: Vec<(&str, bool)> = last
            .sections
            .iter()
            .map(|s| (s.title.as_str(), s.writing))
            .collect();
        assert_eq!(shape, [("Launch Plan", false), ("Globex Pitch", true)]);
    }

    #[tokio::test]
    async fn the_topic_step_starts_a_new_topic_when_the_subject_changes() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        *generator.topic_answer.lock().unwrap() = r#"{"same":false,"topic":"Globex pitch"}"#.into();
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Past the shortest topic, so the step may end it.
        tx.send(speech(180)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        finish.cancel();
        let snapshot = task.await.unwrap();
        let topics: Vec<_> = snapshot
            .entries
            .iter()
            .map(|e| e.topic.as_deref())
            .collect();
        assert_eq!(topics, vec![Some("Launch Plan"), Some("Globex Pitch")]);
        let topic_prompts = generator.topic_prompts.lock().unwrap();
        assert!(
            topic_prompts[0].starts_with("Current topic: \"Launch Plan\" (since 00:00, 3 min)\n")
        );
        assert!(topic_prompts[0].contains("Latest notes under it:\n- "));
    }

    /// Two windows: the first names the topic, the second (past the
    /// shortest topic) asks the topic step.
    async fn two_windows(generator: Arc<FakeGenerator>) -> (TimelineSnapshot, Arc<FakeHost>) {
        let host = host_with(generator);
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(speech(180)).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        finish.cancel();
        (task.await.unwrap(), host)
    }

    #[tokio::test]
    async fn a_failed_topic_step_fails_the_window_and_it_is_retried() {
        for kind in [FailureKind::Preempted, FailureKind::Transient] {
            let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
            *generator.topic_fail.lock().unwrap() = Some(GenerateError {
                kind,
                message: "busy".into(),
            });
            let (snapshot, host) = two_windows(generator.clone()).await;
            // The step was asked again after the failure, and the window's
            // notes were written by the retry (not by a notes call that went
            // ahead without a topic).
            assert!(
                generator.topic_prompts.lock().unwrap().len() >= 2,
                "{kind:?}"
            );
            assert_eq!(snapshot.entries.len(), 2, "{kind:?}");
            let statuses: Vec<_> = host
                .published
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.status)
                .collect();
            if kind == FailureKind::Transient {
                assert!(statuses.contains(&TimelineStatus::Failed));
            }
        }
    }

    #[tokio::test]
    async fn a_refused_topic_step_backs_off_like_a_refused_request() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        *generator.topic_fail.lock().unwrap() = Some(GenerateError {
            kind: FailureKind::Unavailable,
            message: "status 429".into(),
        });
        let (_, host) = two_windows(generator.clone()).await;
        let statuses: Vec<_> = host
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.status)
            .collect();
        assert!(statuses.contains(&TimelineStatus::Unavailable));
        // The refused attempt made no notes call: the first window and the
        // retry after the back-off are the only two.
        assert_eq!(generator.calls.load(Ordering::SeqCst), 2);
        assert!(generator.topic_prompts.lock().unwrap().len() >= 2);
    }

    #[tokio::test]
    async fn a_hung_topic_step_times_out_on_its_own_short_timeout() {
        let mut generator = FakeGenerator::new(Duration::ZERO);
        generator.topic_hang = true;
        let generator = Arc::new(generator);
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let mut config = config();
        config.topic_timeout = Duration::from_millis(30);
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(speech(180)).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        finish.cancel();
        task.await.unwrap();
        let statuses: Vec<_> = host
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.status)
            .collect();
        assert!(statuses.contains(&TimelineStatus::Failed));
        assert!(host.published.lock().unwrap().iter().any(|s| s
            .error
            .as_deref()
            .is_some_and(|e| e.contains("topic step timed out"))));
    }

    #[tokio::test]
    async fn cloud_providers_make_one_request_per_window() {
        let mut generator = FakeGenerator::new(Duration::ZERO);
        generator.provider = "openai";
        let generator = Arc::new(generator);
        let (snapshot, _) = two_windows(generator.clone()).await;
        assert!(generator.topic_prompts.lock().unwrap().is_empty());
        assert_eq!(
            generator.calls.load(Ordering::SeqCst),
            snapshot.entries.len()
        );
        assert!(uses_topic_step("builtin-ai") && !uses_topic_step("claude"));
    }

    #[test]
    fn only_the_live_notes_model_gets_the_compact_prompt() {
        use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
        assert!(uses_compact_prompt("builtin-ai", LIVE_NOTES_MODEL));
        // Existing users on a built-in summary model keep the JSON prompt.
        assert!(!uses_compact_prompt("builtin-ai", "qwen3.5:4b"));
        assert!(!uses_compact_prompt("builtin-ai", "qwen3.5:2b"));
        assert!(!uses_compact_prompt("ollama", LIVE_NOTES_MODEL));
        assert!(!uses_compact_prompt("claude", "claude-sonnet"));
        let window = Window {
            start_ms: 0,
            end_ms: 60_000,
            segments: Vec::new(),
            dropped_segments: 0,
            max_entries: 3,
        };
        let context = PromptContext::default();
        assert_eq!(
            notes_prompts("builtin-ai", "qwen3.5:4b", &window, &context).0,
            prompt::system_prompt()
        );
        assert_eq!(
            notes_prompts("builtin-ai", LIVE_NOTES_MODEL, &window, &context).0,
            prompt::compact_system_prompt()
        );
    }

    #[tokio::test]
    async fn the_tail_window_at_stop_skips_the_topic_step() {
        let generator = Arc::new(FakeGenerator::new(Duration::ZERO));
        let host = host_with(generator.clone());
        let (tx, rx) = unbounded_channel();
        let finish = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), "s".into(), rx, finish.clone(), config()));
        tx.send(speech(0)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Speech that only the stop's flush picks up.
        tx.send(speech(240)).unwrap();
        finish.cancel();
        let snapshot = task.await.unwrap();
        assert_eq!(snapshot.entries.len(), 2);
        assert!(generator.topic_prompts.lock().unwrap().is_empty());
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
