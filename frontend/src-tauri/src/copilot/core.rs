//! One recording session's Copilot: the transcript it has heard, its
//! messages, the proactive detector and every model request. The Tauri glue
//! (`mod.rs`) only forwards lines and commands, so this is tested with a fake
//! host and a fake model.
//!
//! Nothing here can affect recording: requests run on their own tasks, errors
//! only change a message, and a finished session refuses new work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::context::{self, Line, Speaker, TimelineItem};
use super::detector::{parse_yes_no, rule_check};
use super::prompt::{strip_reasoning, QuickAction, Task};
use super::stream::{StreamError, StreamErrorKind, StreamOutcome, Streamer};

/// Timings, overridable in tests.
#[derive(Debug, Clone, Copy)]
pub struct CoreConfig {
    /// No first word within this: the request fails.
    pub first_token_timeout: Duration,
    /// No new words for this long mid-answer: the request fails.
    pub stall_timeout: Duration,
    /// Minimum time between two suggestion cards.
    pub card_cooldown: Duration,
    /// A card stops counting as "active" after this, so suggestions resume
    /// even if the user never dismisses it.
    pub card_ttl: Duration,
    /// Minimum time between two proactive confirm calls.
    pub confirm_gap: Duration,
    /// Proactive suggestions pause this long after an auth or rate-limit error.
    pub refusal_pause: Duration,
    /// Live transcription backlog above which proactive work is shed.
    pub max_live_backlog_secs: f64,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            first_token_timeout: Duration::from_secs(15),
            stall_timeout: Duration::from_secs(15),
            card_cooldown: Duration::from_secs(20),
            card_ttl: Duration::from_secs(120),
            confirm_gap: Duration::from_secs(8),
            refusal_pause: Duration::from_secs(120),
            max_live_backlog_secs: 2.0,
        }
    }
}

/// What Copilot needs from the app.
#[async_trait]
pub trait Host: Send + Sync + 'static {
    fn emit_state(&self, snapshot: &Snapshot);
    fn emit_delta(&self, delta: &Delta);
    fn timeline(&self) -> Vec<TimelineItem>;
    fn suggestions_enabled(&self) -> bool;
    fn debug_enabled(&self) -> bool;
    /// The recording is paused: no proactive work.
    fn is_paused(&self) -> bool;
    /// Seconds of audio live transcription has queued (0 when idle).
    fn live_backlog_secs(&self) -> f64;
    /// The model to use; `force_builtin` for "Use built-in model instead".
    async fn streamer(&self, force_builtin: bool) -> Result<Arc<dyn Streamer>, String>;
    /// Writes finished moments. `Ok(false)`: the meeting is not bound yet.
    async fn persist(&self, rows: &[ChatRow]) -> Result<bool, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Ask,
    Action,
    Suggestion,
}

impl MessageKind {
    fn stored(self) -> &'static str {
        match self {
            MessageKind::Ask => "live_ask",
            MessageKind::Action => "live_action",
            MessageKind::Suggestion => "live_suggestion",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Streaming,
    Done,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Active,
    Dismissed,
    NotForMe,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
    pub tokens_per_second: Option<f64>,
    pub prompt_tokens: Option<u32>,
    pub reused_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub kind: MessageKind,
    /// The quick action, for `Action` messages.
    pub action: Option<QuickAction>,
    /// What the user asked, the action's label, or the quoted line.
    pub prompt: String,
    /// The moment in the recording this belongs to.
    pub audio_ms: i64,
    pub reply: String,
    pub status: MessageStatus,
    pub error: Option<String>,
    /// A hosted provider failed: offer the built-in model.
    pub can_use_builtin: bool,
    pub card_state: Option<CardState>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub stats: Option<Stats>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectorStats {
    pub rule_pass: u32,
    pub rule_reject: u32,
    pub confirm_yes: u32,
    pub confirm_no: u32,
    pub skipped: u32,
    pub false_alarms: u32,
    /// The last decision, for the debug line ("rule: question mark", …).
    pub last: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStats {
    pub last: Option<Stats>,
    pub detector: DetectorStats,
    pub live_backlog_secs: f64,
}

/// `copilot-state` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session_id: String,
    pub active: bool,
    pub line_count: usize,
    pub messages: Vec<Message>,
    pub suggestions_enabled: bool,
    pub debug_enabled: bool,
    pub debug: DebugStats,
}

/// `copilot-delta` payload: `delta` goes at byte `offset` of the reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    pub session_id: String,
    pub id: String,
    pub offset: usize,
    pub delta: String,
}

/// A row for `meeting_chat_messages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRow {
    pub id: String,
    pub role: &'static str,
    pub content: String,
    pub created_at: String,
    pub kind: &'static str,
    pub label: Option<String>,
    pub audio_ms: i64,
}

/// The two rows a finished moment becomes.
pub fn rows_for(message: &Message, answered_at: &str) -> Vec<ChatRow> {
    let (content, label) = match message.kind {
        MessageKind::Ask => (message.prompt.clone(), None),
        MessageKind::Action => (
            message.prompt.clone(),
            message.action.map(|a| a.label().to_string()),
        ),
        MessageKind::Suggestion => (
            format!("\"{}\"", message.prompt),
            Some("Others asked you".to_string()),
        ),
    };
    vec![
        ChatRow {
            id: format!("{}-q", message.id),
            role: "user",
            content,
            created_at: message.created_at.clone(),
            kind: message.kind.stored(),
            label: label.clone(),
            audio_ms: message.audio_ms,
        },
        ChatRow {
            id: format!("{}-a", message.id),
            role: "assistant",
            content: message.reply.trim().to_string(),
            created_at: answered_at.to_string(),
            kind: message.kind.stored(),
            label,
            audio_ms: message.audio_ms,
        },
    ]
}

struct State {
    active: bool,
    lines: Vec<Line>,
    notes: String,
    messages: Vec<Message>,
    /// The task each message ran, for Retry.
    tasks: HashMap<String, Task>,
    in_flight: HashMap<String, CancellationToken>,
    /// Messages the user stopped (vs. timed out or ended).
    stopped: Vec<String>,
    last_card_at: Option<Instant>,
    last_confirm_at: Option<Instant>,
    detector_busy: bool,
    paused_until: Option<Instant>,
    unsaved: Vec<ChatRow>,
    debug: DebugStats,
}

pub struct Core {
    session_id: String,
    host: Arc<dyn Host>,
    config: CoreConfig,
    state: Mutex<State>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Core {
    pub fn new(session_id: String, host: Arc<dyn Host>, config: CoreConfig) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            host,
            config,
            state: Mutex::new(State {
                active: true,
                lines: Vec::new(),
                notes: String::new(),
                messages: Vec::new(),
                tasks: HashMap::new(),
                in_flight: HashMap::new(),
                stopped: Vec::new(),
                last_card_at: None,
                last_confirm_at: None,
                detector_busy: false,
                paused_until: None,
                unsaved: Vec::new(),
                debug: DebugStats::default(),
            }),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        let mut debug = state.debug.clone();
        debug.live_backlog_secs = self.host.live_backlog_secs();
        Snapshot {
            session_id: self.session_id.clone(),
            active: state.active,
            line_count: state.lines.len(),
            messages: state.messages.clone(),
            suggestions_enabled: self.host.suggestions_enabled(),
            debug_enabled: self.host.debug_enabled(),
            debug,
        }
    }

    fn publish(&self) {
        self.host.emit_state(&self.snapshot());
    }

    pub fn is_active(&self) -> bool {
        self.lock().active
    }

    pub fn set_notes(&self, notes: String) {
        self.lock().notes = notes;
    }

    /// A finalized live transcript line.
    pub fn on_line(self: &Arc<Self>, line: Line) {
        let candidate = {
            let mut state = self.lock();
            if !state.active {
                return;
            }
            let first = state.lines.is_empty();
            context::push_line(&mut state.lines, line.clone());
            drop(state);
            if first {
                self.publish();
            }
            line.speaker == Speaker::Others && self.host.suggestions_enabled()
        };
        if candidate {
            self.consider_suggestion(line);
        }
    }

    fn record_decision(&self, state: &mut State, text: String) {
        perf_debug!("Copilot detector: {}", text);
        state.debug.detector.last = Some(text);
    }

    /// The rule check, then (rate-limited and only when the machine keeps up)
    /// a one-word model confirmation, then the draft.
    fn consider_suggestion(self: &Arc<Self>, line: Line) {
        let decision = rule_check(&line.text);
        let now = Instant::now();
        {
            let mut state = self.lock();
            if !decision.passed() {
                state.debug.detector.rule_reject += 1;
                self.record_decision(&mut state, format!("rule: no ({})", decision.reason()));
                return;
            }
            state.debug.detector.rule_pass += 1;
            let card_active = state.messages.iter().any(|m| {
                m.card_state == Some(CardState::Active)
                    && state
                        .last_card_at
                        .is_some_and(|at| now.duration_since(at) < self.config.card_ttl)
            });
            let backlog = self.host.live_backlog_secs();
            let skip = if self.host.is_paused() {
                Some("paused".to_string())
            } else if card_active {
                Some("a card is open".to_string())
            } else if state
                .last_card_at
                .is_some_and(|at| now.duration_since(at) < self.config.card_cooldown)
            {
                Some("cooldown".to_string())
            } else if state.detector_busy {
                Some("busy".to_string())
            } else if state
                .last_confirm_at
                .is_some_and(|at| now.duration_since(at) < self.config.confirm_gap)
            {
                Some("rate limit".to_string())
            } else if state.paused_until.is_some_and(|until| now < until) {
                Some("provider refused".to_string())
            } else if backlog > self.config.max_live_backlog_secs {
                Some(format!("live transcription {backlog:.1}s behind"))
            } else {
                None
            };
            if let Some(reason) = skip {
                state.debug.detector.skipped += 1;
                self.record_decision(
                    &mut state,
                    format!("rule: yes ({}), skipped: {reason}", decision.reason()),
                );
                return;
            }
            state.detector_busy = true;
            state.last_confirm_at = Some(now);
            self.record_decision(
                &mut state,
                format!("rule: yes ({}), confirming", decision.reason()),
            );
        }
        let core = self.clone();
        tokio::spawn(async move {
            let confirmed = core.confirm(&line).await;
            core.lock().detector_busy = false;
            if confirmed {
                core.open_card(line);
            }
            core.publish();
        });
    }

    async fn confirm(&self, line: &Line) -> bool {
        let task = Task::Confirm {
            at_ms: line.start_ms,
            line: line.text.clone(),
        };
        let Ok(streamer) = self.host.streamer(false).await else {
            return false;
        };
        let parts = self.prompt_for(&task);
        let cancel = CancellationToken::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = tokio::time::timeout(
            self.config.first_token_timeout,
            streamer.stream(&parts, task.max_tokens(), task.temperature(), tx, &cancel),
        )
        .await;
        cancel.cancel();
        let mut state = self.lock();
        match result {
            Ok(Ok(outcome)) => {
                let yes = parse_yes_no(&outcome.text);
                if yes {
                    state.debug.detector.confirm_yes += 1;
                } else {
                    state.debug.detector.confirm_no += 1;
                }
                let verdict = if yes { "yes" } else { "no" };
                self.record_decision(&mut state, format!("model: {verdict}"));
                yes && state.active
            }
            Ok(Err(error)) => {
                if matches!(
                    error.kind,
                    StreamErrorKind::Auth | StreamErrorKind::RateLimited
                ) {
                    state.paused_until = Some(Instant::now() + self.config.refusal_pause);
                }
                self.record_decision(&mut state, format!("model: failed ({})", error.message));
                false
            }
            Err(_) => {
                self.record_decision(&mut state, "model: timed out".into());
                false
            }
        }
    }

    fn open_card(self: &Arc<Self>, line: Line) {
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.lock();
            if !state.active {
                return;
            }
            state.last_card_at = Some(Instant::now());
            state.messages.push(Message {
                id: id.clone(),
                kind: MessageKind::Suggestion,
                action: None,
                prompt: line.text.trim().to_string(),
                audio_ms: line.start_ms,
                reply: String::new(),
                status: MessageStatus::Pending,
                error: None,
                can_use_builtin: false,
                card_state: Some(CardState::Active),
                provider: None,
                model: None,
                stats: None,
                created_at: now_rfc3339(),
            });
        }
        self.start(
            id,
            Task::Suggest {
                at_ms: line.start_ms,
                line: line.text,
            },
            false,
        );
    }

    fn latest_ms(state: &State) -> i64 {
        state.lines.iter().map(|l| l.end_ms).max().unwrap_or(0)
    }

    /// A quick action or a question from the user. Returns the message id.
    pub fn ask(
        self: &Arc<Self>,
        action: Option<QuickAction>,
        question: Option<String>,
    ) -> Result<String, String> {
        let (kind, prompt, task) = match (action, question.map(|q| q.trim().to_string())) {
            (Some(action), _) if action != QuickAction::Shorter => (
                MessageKind::Action,
                action.label().to_string(),
                Task::Action(action),
            ),
            (_, Some(question)) if !question.is_empty() => {
                (MessageKind::Ask, question.clone(), Task::Question(question))
            }
            _ => return Err("Ask a question or pick an action".into()),
        };
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = self.lock();
            if !state.active {
                return Err("The meeting has ended".into());
            }
            if state.lines.is_empty() {
                return Err("Copilot starts once the first words are transcribed".into());
            }
            let audio_ms = Self::latest_ms(&state);
            state.messages.push(Message {
                id: id.clone(),
                kind,
                action,
                prompt,
                audio_ms,
                reply: String::new(),
                status: MessageStatus::Pending,
                error: None,
                can_use_builtin: false,
                card_state: None,
                provider: None,
                model: None,
                stats: None,
                created_at: now_rfc3339(),
            });
        }
        self.start(id.clone(), task, false);
        Ok(id)
    }

    /// "Shorter": a new message that shortens `id`'s answer.
    pub fn shorter(self: &Arc<Self>, id: &str) -> Result<String, String> {
        let new_id = uuid::Uuid::new_v4().to_string();
        let previous = {
            let mut state = self.lock();
            if !state.active {
                return Err("The meeting has ended".into());
            }
            let source = state
                .messages
                .iter()
                .find(|m| m.id == id)
                .filter(|m| !m.reply.trim().is_empty())
                .ok_or("There is no answer to shorten yet")?;
            let previous = source.reply.clone();
            let audio_ms = source.audio_ms;
            state.messages.push(Message {
                id: new_id.clone(),
                kind: MessageKind::Action,
                action: Some(QuickAction::Shorter),
                prompt: QuickAction::Shorter.label().to_string(),
                audio_ms,
                reply: String::new(),
                status: MessageStatus::Pending,
                error: None,
                can_use_builtin: false,
                card_state: None,
                provider: None,
                model: None,
                stats: None,
                created_at: now_rfc3339(),
            });
            previous
        };
        self.start(new_id.clone(), Task::Shorten { previous }, false);
        Ok(new_id)
    }

    pub fn retry(self: &Arc<Self>, id: &str, use_builtin: bool) -> Result<(), String> {
        let task = {
            let mut state = self.lock();
            if !state.active {
                return Err("The meeting has ended".into());
            }
            if state.in_flight.contains_key(id) {
                return Err("Copilot is still answering".into());
            }
            let task = state.tasks.get(id).cloned().ok_or("Nothing to retry")?;
            let message = state
                .messages
                .iter_mut()
                .find(|m| m.id == id)
                .ok_or("Nothing to retry")?;
            message.reply.clear();
            message.error = None;
            message.status = MessageStatus::Pending;
            message.can_use_builtin = false;
            task
        };
        self.start(id.to_string(), task, use_builtin);
        Ok(())
    }

    pub fn stop(&self, id: &str) {
        let mut state = self.lock();
        if let Some(token) = state.in_flight.get(id).cloned() {
            state.stopped.push(id.to_string());
            token.cancel();
        }
    }

    pub fn dismiss_card(&self, id: &str, not_for_me: bool) {
        {
            let mut state = self.lock();
            let Some(message) = state.messages.iter_mut().find(|m| m.id == id) else {
                return;
            };
            if message.card_state != Some(CardState::Active) {
                return;
            }
            message.card_state = Some(if not_for_me {
                CardState::NotForMe
            } else {
                CardState::Dismissed
            });
            if not_for_me {
                state.debug.detector.false_alarms += 1;
            }
            if let Some(token) = state.in_flight.get(id).cloned() {
                if not_for_me {
                    state.stopped.push(id.to_string());
                    token.cancel();
                }
            }
        }
        self.publish();
    }

    fn prompt_for(&self, task: &Task) -> context::PromptParts {
        let timeline = self.host.timeline();
        let state = self.lock();
        context::build(&state.lines, &timeline, &state.notes, task)
    }

    fn update_message(&self, id: &str, change: impl FnOnce(&mut Message)) {
        let mut state = self.lock();
        if let Some(message) = state.messages.iter_mut().find(|m| m.id == id) {
            change(message);
        }
    }

    /// Runs `task` for message `id` on its own task.
    fn start(self: &Arc<Self>, id: String, task: Task, force_builtin: bool) {
        let cancel = CancellationToken::new();
        {
            let mut state = self.lock();
            state.tasks.insert(id.clone(), task.clone());
            state.in_flight.insert(id.clone(), cancel.clone());
            state.stopped.retain(|s| s != &id);
        }
        self.publish();
        let core = self.clone();
        tokio::spawn(async move {
            core.run(id, task, force_builtin, cancel).await;
        });
    }

    async fn run(
        self: Arc<Self>,
        id: String,
        task: Task,
        force_builtin: bool,
        cancel: CancellationToken,
    ) {
        let started = Instant::now();
        let streamer = match self.host.streamer(force_builtin).await {
            Ok(streamer) => streamer,
            Err(error) => {
                self.finish_message(
                    &id,
                    Err(StreamError::new(StreamErrorKind::Other, error)),
                    false,
                    None,
                    started,
                );
                return;
            }
        };
        let is_cloud = streamer.is_cloud();
        let (provider, model) = (
            streamer.provider().to_string(),
            streamer.model().to_string(),
        );
        self.update_message(&id, |m| {
            m.provider = Some(provider);
            m.model = Some(model);
            m.status = MessageStatus::Streaming;
        });
        self.publish();

        let parts = self.prompt_for(&task);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let max_tokens = task.max_tokens();
        let temperature = task.temperature();
        let stream_cancel = cancel.clone();
        let mut handle = tokio::spawn(async move {
            streamer
                .stream(&parts, max_tokens, temperature, tx, &stream_cancel)
                .await
        });

        let mut first_token: Option<Duration> = None;
        let mut last_token = started;
        let mut timed_out = false;
        let mut rx_closed = false;
        let result = loop {
            let deadline = match first_token {
                None => started + self.config.first_token_timeout,
                Some(_) => last_token + self.config.stall_timeout,
            };
            tokio::select! {
                biased;
                delta = rx.recv(), if !timed_out && !rx_closed => {
                    match delta {
                        Some(delta) => {
                            first_token.get_or_insert_with(|| started.elapsed());
                            last_token = Instant::now();
                            self.append(&id, delta);
                        }
                        None => rx_closed = true,
                    }
                }
                joined = &mut handle => {
                    while let Ok(delta) = rx.try_recv() {
                        first_token.get_or_insert_with(|| started.elapsed());
                        self.append(&id, delta);
                    }
                    break joined.unwrap_or_else(|e| Err(StreamError::new(StreamErrorKind::Other, format!("Copilot's request task failed: {e}"))));
                }
                _ = tokio::time::sleep_until(deadline), if !timed_out => {
                    timed_out = true;
                    cancel.cancel();
                }
            }
        };
        let user_stopped = {
            let mut state = self.lock();
            state.in_flight.remove(&id);
            state.stopped.contains(&id)
        };
        let result = match result {
            Err(error)
                if error.kind == StreamErrorKind::Cancelled && timed_out && !user_stopped =>
            {
                Err(StreamError::new(
                    StreamErrorKind::Other,
                    if first_token.is_none() {
                        format!(
                            "No answer within {} seconds.",
                            self.config.first_token_timeout.as_secs()
                        )
                    } else {
                        "The answer stalled.".to_string()
                    },
                ))
            }
            other => other,
        };
        self.finish_message(&id, result, is_cloud, first_token, started);
    }

    fn append(&self, id: &str, delta: String) {
        if delta.is_empty() {
            return;
        }
        let event = {
            let mut state = self.lock();
            let Some(message) = state.messages.iter_mut().find(|m| m.id == id) else {
                return;
            };
            let offset = message.reply.len();
            message.reply.push_str(&delta);
            Delta {
                session_id: self.session_id.clone(),
                id: id.to_string(),
                offset,
                delta,
            }
        };
        self.host.emit_delta(&event);
    }

    fn finish_message(
        self: &Arc<Self>,
        id: &str,
        result: Result<StreamOutcome, StreamError>,
        is_cloud: bool,
        first_token: Option<Duration>,
        started: Instant,
    ) {
        let rows = {
            let mut state = self.lock();
            state.in_flight.remove(id);
            let user_stopped = state.stopped.contains(&id.to_string());
            let mut refused = false;
            let Some(message) = state.messages.iter_mut().find(|m| m.id == id) else {
                return;
            };
            match result {
                Ok(outcome) => {
                    let text = strip_reasoning(if outcome.text.trim().is_empty() {
                        &message.reply
                    } else {
                        &outcome.text
                    });
                    message.reply = text;
                    message.status = MessageStatus::Done;
                    message.stats = Some(Stats {
                        first_token_ms: first_token.map(|d| d.as_millis() as u64),
                        total_ms: started.elapsed().as_millis() as u64,
                        tokens_per_second: outcome.tokens_per_second.or_else(|| {
                            let secs = started
                                .elapsed()
                                .saturating_sub(first_token.unwrap_or_default())
                                .as_secs_f64();
                            outcome
                                .completion_tokens
                                .filter(|_| secs > 0.0)
                                .map(|n| f64::from(n) / secs)
                        }),
                        prompt_tokens: outcome.prompt_tokens,
                        reused_tokens: outcome.reused_tokens,
                    });
                }
                Err(error) if error.kind == StreamErrorKind::Cancelled || user_stopped => {
                    message.reply = strip_reasoning(&message.reply);
                    message.status = MessageStatus::Stopped;
                }
                Err(error) => {
                    refused = matches!(
                        error.kind,
                        StreamErrorKind::Auth | StreamErrorKind::RateLimited
                    );
                    message.status = MessageStatus::Error;
                    message.error = Some(error.message);
                    message.can_use_builtin = is_cloud;
                }
            }
            let stats = message.stats.clone();
            let keep = !message.reply.trim().is_empty()
                && matches!(message.status, MessageStatus::Done | MessageStatus::Stopped)
                && message.card_state != Some(CardState::NotForMe);
            let rows = keep.then(|| rows_for(message, &now_rfc3339()));
            if stats.is_some() {
                state.debug.last = stats;
            }
            if refused {
                state.paused_until = Some(Instant::now() + self.config.refusal_pause);
            }
            if let Some(rows) = rows {
                state.unsaved.extend(rows);
            }
            !state.unsaved.is_empty()
        };
        self.publish();
        if rows {
            let core = self.clone();
            tokio::spawn(async move {
                core.persist_unsaved().await;
            });
        }
    }

    /// Writes waiting rows. Returns true when nothing is left to write.
    pub async fn persist_unsaved(&self) -> bool {
        let rows = std::mem::take(&mut self.lock().unsaved);
        if rows.is_empty() {
            return true;
        }
        match self.host.persist(&rows).await {
            Ok(true) => true,
            Ok(false) => {
                // No meeting id yet: keep them for later.
                self.lock().unsaved.splice(0..0, rows);
                false
            }
            Err(error) => {
                log::warn!("Copilot: could not save {} rows: {error}", rows.len());
                self.lock().unsaved.splice(0..0, rows);
                false
            }
        }
    }

    /// The meeting ended: refuse new work, stop what is running, and write
    /// what is left (retrying for `persist_window` while the meeting id binds).
    pub async fn finish(self: &Arc<Self>, persist_window: Duration, retry: Duration) {
        let tokens: Vec<CancellationToken> = {
            let mut state = self.lock();
            state.active = false;
            state.in_flight.values().cloned().collect()
        };
        for token in tokens {
            token.cancel();
        }
        self.publish();
        let deadline = Instant::now() + persist_window;
        // Let cancelled requests record what they had.
        while !self.lock().in_flight.is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        loop {
            if self.persist_unsaved().await || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(retry).await;
        }
        let left = self.lock().unsaved.len();
        if left > 0 {
            log::warn!("Copilot: {left} rows could not be saved before giving up");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::mpsc::UnboundedSender;

    struct FakeStreamer {
        calls: Arc<AtomicUsize>,
        confirm_reply: &'static str,
        delay: Duration,
        fail: Option<StreamErrorKind>,
        cloud: bool,
    }

    #[async_trait]
    impl Streamer for FakeStreamer {
        fn provider(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-model"
        }
        fn is_cloud(&self) -> bool {
            self.cloud
        }
        async fn stream(
            &self,
            parts: &context::PromptParts,
            max_tokens: u32,
            _temperature: f32,
            deltas: UnboundedSender<String>,
            cancel: &CancellationToken,
        ) -> Result<StreamOutcome, StreamError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(kind) = self.fail {
                return Err(StreamError::new(kind, "refused"));
            }
            if max_tokens == 5 {
                return Ok(StreamOutcome {
                    text: self.confirm_reply.into(),
                    ..Default::default()
                });
            }
            let mut text = String::new();
            for word in ["Say ", "we ", "ship ", "Friday ", "[00:05]."] {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(StreamError::new(StreamErrorKind::Cancelled, "Stopped")),
                    _ = tokio::time::sleep(self.delay) => {}
                }
                text.push_str(word);
                let _ = deltas.send(word.to_string());
            }
            assert!(parts.rest.contains("TASK:"));
            Ok(StreamOutcome {
                text,
                prompt_tokens: Some(1200),
                reused_tokens: Some(1100),
                tokens_per_second: Some(20.0),
                ..Default::default()
            })
        }
    }

    struct FakeHost {
        calls: Arc<AtomicUsize>,
        confirm_reply: &'static str,
        delay: Duration,
        fail: Option<StreamErrorKind>,
        cloud: bool,
        suggestions: AtomicBool,
        meeting_bound: AtomicBool,
        backlog: Mutex<f64>,
        persisted: Mutex<Vec<ChatRow>>,
        states: Mutex<Vec<Snapshot>>,
        deltas: Mutex<Vec<Delta>>,
        builtin_requests: AtomicUsize,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                confirm_reply: "yes",
                delay: Duration::from_millis(5),
                fail: None,
                cloud: false,
                suggestions: AtomicBool::new(true),
                meeting_bound: AtomicBool::new(true),
                backlog: Mutex::new(0.0),
                persisted: Mutex::new(Vec::new()),
                states: Mutex::new(Vec::new()),
                deltas: Mutex::new(Vec::new()),
                builtin_requests: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Host for FakeHost {
        fn emit_state(&self, snapshot: &Snapshot) {
            self.states.lock().unwrap().push(snapshot.clone());
        }
        fn emit_delta(&self, delta: &Delta) {
            self.deltas.lock().unwrap().push(delta.clone());
        }
        fn timeline(&self) -> Vec<TimelineItem> {
            Vec::new()
        }
        fn suggestions_enabled(&self) -> bool {
            self.suggestions.load(Ordering::SeqCst)
        }
        fn debug_enabled(&self) -> bool {
            false
        }
        fn is_paused(&self) -> bool {
            false
        }
        fn live_backlog_secs(&self) -> f64 {
            *self.backlog.lock().unwrap()
        }
        async fn streamer(&self, force_builtin: bool) -> Result<Arc<dyn Streamer>, String> {
            if force_builtin {
                self.builtin_requests.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Arc::new(FakeStreamer {
                calls: self.calls.clone(),
                confirm_reply: self.confirm_reply,
                delay: self.delay,
                fail: if force_builtin { None } else { self.fail },
                cloud: self.cloud && !force_builtin,
            }))
        }
        async fn persist(&self, rows: &[ChatRow]) -> Result<bool, String> {
            if !self.meeting_bound.load(Ordering::SeqCst) {
                return Ok(false);
            }
            self.persisted.lock().unwrap().extend(rows.iter().cloned());
            Ok(true)
        }
    }

    fn config() -> CoreConfig {
        CoreConfig {
            first_token_timeout: Duration::from_millis(300),
            stall_timeout: Duration::from_millis(300),
            card_cooldown: Duration::from_millis(400),
            card_ttl: Duration::from_secs(60),
            confirm_gap: Duration::ZERO,
            refusal_pause: Duration::from_secs(60),
            max_live_backlog_secs: 2.0,
        }
    }

    fn line(start_s: i64, speaker: Speaker, text: &str) -> Line {
        Line {
            start_ms: start_s * 1000,
            end_ms: start_s * 1000 + 2000,
            speaker,
            text: text.into(),
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    fn core_with(host: FakeHost) -> (Arc<FakeHost>, Arc<Core>) {
        let host = Arc::new(host);
        let core = Core::new("s1".into(), host.clone(), config());
        (host, core)
    }

    #[tokio::test]
    async fn idle_makes_no_requests() {
        let (host, core) = core_with(FakeHost::new());
        core.on_line(line(1, Speaker::Others, "The numbers look fine to me."));
        core.on_line(line(5, Speaker::You, "Can you hear me?"));
        settle().await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
        assert!(core.snapshot().messages.is_empty());
    }

    #[tokio::test]
    async fn asking_needs_a_first_line_and_streams_a_cited_answer() {
        let (host, core) = core_with(FakeHost::new());
        assert!(core.ask(Some(QuickAction::CatchUp), None).is_err());
        core.on_line(line(5, Speaker::You, "We should ship on Friday."));
        let id = core.ask(Some(QuickAction::WhatToSay), None).unwrap();
        settle().await;
        let snapshot = core.snapshot();
        let message = snapshot.messages.iter().find(|m| m.id == id).unwrap();
        assert_eq!(message.status, MessageStatus::Done);
        assert_eq!(message.reply, "Say we ship Friday [00:05].");
        assert_eq!(message.stats.as_ref().unwrap().reused_tokens, Some(1100));
        // Deltas line up with their offsets.
        let deltas = host.deltas.lock().unwrap();
        let mut rebuilt = String::new();
        for delta in deltas.iter().filter(|d| d.id == id) {
            assert_eq!(delta.offset, rebuilt.len());
            rebuilt.push_str(&delta.delta);
        }
        assert_eq!(rebuilt, message.reply);
        // Saved as a moment in the chat thread.
        let rows = host.persisted.lock().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[0].kind, "live_action");
        assert_eq!(rows[0].label.as_deref(), Some("What should I say?"));
        assert_eq!(rows[0].audio_ms, 7000);
        assert_eq!(rows[1].content, "Say we ship Friday [00:05].");
    }

    #[tokio::test]
    async fn a_question_from_others_opens_one_card_and_never_on_your_own_lines() {
        let (host, core) = core_with(FakeHost::new());
        core.on_line(line(1, Speaker::You, "What do you think about Friday?"));
        settle().await;
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            0,
            "own lines never trigger"
        );

        core.on_line(line(
            4,
            Speaker::Others,
            "Can you send me the deck after this?",
        ));
        settle().await;
        let snapshot = core.snapshot();
        let cards: Vec<_> = snapshot
            .messages
            .iter()
            .filter(|m| m.kind == MessageKind::Suggestion)
            .collect();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].card_state, Some(CardState::Active));
        assert_eq!(cards[0].audio_ms, 4000);
        assert_eq!(cards[0].status, MessageStatus::Done);
        assert_eq!(snapshot.debug.detector.confirm_yes, 1);

        // A second question while the card is open: no second card.
        core.on_line(line(8, Speaker::Others, "And what about the budget?"));
        settle().await;
        assert_eq!(
            core.snapshot()
                .messages
                .iter()
                .filter(|m| m.kind == MessageKind::Suggestion)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cooldown_and_not_for_me() {
        let (_host, core) = core_with(FakeHost::new());
        core.on_line(line(1, Speaker::Others, "Can you send me the deck?"));
        settle().await;
        let card = core.snapshot().messages[0].id.clone();
        core.dismiss_card(&card, true);
        assert_eq!(core.snapshot().debug.detector.false_alarms, 1);
        // Within the cooldown: skipped even though the card is closed.
        core.on_line(line(5, Speaker::Others, "Could you share the numbers?"));
        settle().await;
        assert_eq!(core.snapshot().messages.len(), 1);
        assert!(core
            .snapshot()
            .debug
            .detector
            .last
            .unwrap()
            .contains("cooldown"));
        // After it: a new card.
        tokio::time::sleep(Duration::from_millis(400)).await;
        core.on_line(line(30, Speaker::Others, "Could you share the numbers?"));
        settle().await;
        assert_eq!(core.snapshot().messages.len(), 2);
    }

    #[tokio::test]
    async fn the_model_saying_no_opens_nothing() {
        let mut host = FakeHost::new();
        host.confirm_reply = "No";
        let (host, core) = core_with(host);
        core.on_line(line(1, Speaker::Others, "Can you believe this weather?"));
        settle().await;
        assert!(core.snapshot().messages.is_empty());
        assert_eq!(core.snapshot().debug.detector.confirm_no, 1);
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            1,
            "only the tiny confirm call"
        );
    }

    #[tokio::test]
    async fn suggestions_back_off_when_live_transcription_falls_behind() {
        let (host, core) = core_with(FakeHost::new());
        *host.backlog.lock().unwrap() = 6.0;
        core.on_line(line(1, Speaker::Others, "Can you send me the deck?"));
        settle().await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
        assert!(core
            .snapshot()
            .debug
            .detector
            .last
            .unwrap()
            .contains("behind"));
        // The user can still ask.
        core.ask(Some(QuickAction::Explain), None).unwrap();
        settle().await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn suggestions_off_means_no_detector_calls() {
        let host = FakeHost::new();
        host.suggestions.store(false, Ordering::SeqCst);
        let (host, core) = core_with(host);
        core.on_line(line(1, Speaker::Others, "Can you send me the deck?"));
        settle().await;
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stop_keeps_the_partial_answer() {
        let mut host = FakeHost::new();
        host.delay = Duration::from_millis(60);
        let (host, core) = core_with(host);
        core.on_line(line(5, Speaker::You, "Hello"));
        let id = core.ask(None, Some("What's next?".into())).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        core.stop(&id);
        settle().await;
        let message = core.snapshot().messages[0].clone();
        assert_eq!(message.status, MessageStatus::Stopped);
        assert!(!message.reply.is_empty());
        assert_eq!(
            host.persisted.lock().unwrap().len(),
            2,
            "a stopped answer is still kept"
        );
    }

    #[tokio::test]
    async fn a_slow_model_times_out_with_a_reason() {
        let mut host = FakeHost::new();
        host.delay = Duration::from_millis(500);
        let (_host, core) = core_with(host);
        core.on_line(line(5, Speaker::You, "Hello"));
        core.ask(Some(QuickAction::CatchUp), None).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let message = core.snapshot().messages[0].clone();
        assert_eq!(message.status, MessageStatus::Error);
        assert!(message.error.unwrap().contains("No answer within"));
    }

    #[tokio::test]
    async fn a_refusing_cloud_provider_offers_the_builtin_model_and_pauses_suggestions() {
        let mut host = FakeHost::new();
        host.fail = Some(StreamErrorKind::RateLimited);
        host.cloud = true;
        let (host, core) = core_with(host);
        core.on_line(line(5, Speaker::You, "Hello"));
        let id = core.ask(Some(QuickAction::CatchUp), None).unwrap();
        settle().await;
        let message = core.snapshot().messages[0].clone();
        assert_eq!(message.status, MessageStatus::Error);
        assert!(message.can_use_builtin);
        assert_eq!(host.calls.load(Ordering::SeqCst), 1, "no automatic retries");
        // Proactive work pauses.
        core.on_line(line(9, Speaker::Others, "Can you send it?"));
        settle().await;
        assert!(core
            .snapshot()
            .debug
            .detector
            .last
            .unwrap()
            .contains("refused"));
        // "Use built-in model instead".
        core.retry(&id, true).unwrap();
        settle().await;
        assert_eq!(host.builtin_requests.load(Ordering::SeqCst), 1);
        assert_eq!(core.snapshot().messages[0].status, MessageStatus::Done);
    }

    #[tokio::test]
    async fn nothing_runs_after_the_meeting_ends_and_late_rows_are_saved() {
        let host = FakeHost::new();
        host.meeting_bound.store(false, Ordering::SeqCst);
        let (host, core) = core_with(host);
        core.on_line(line(5, Speaker::You, "Hello"));
        core.ask(Some(QuickAction::CatchUp), None).unwrap();
        settle().await;
        assert!(
            host.persisted.lock().unwrap().is_empty(),
            "no meeting id yet"
        );
        host.meeting_bound.store(true, Ordering::SeqCst);
        core.finish(Duration::from_millis(500), Duration::from_millis(20))
            .await;
        assert_eq!(host.persisted.lock().unwrap().len(), 2);
        let calls = host.calls.load(Ordering::SeqCst);
        assert!(core.ask(Some(QuickAction::CatchUp), None).is_err());
        core.on_line(line(20, Speaker::Others, "Can you send it?"));
        settle().await;
        assert_eq!(host.calls.load(Ordering::SeqCst), calls);
        assert!(!core.snapshot().active);
    }
}
