//! The live Copilot of the replay viewers: asks the real Copilot about a run
//! as it stood at a cursor, whatever carries the answer (`replay serve`'s
//! server-sent events or the app's `replay-copilot` events).
//!
//! The answer's progress goes to a sink as `(event, data)`: `queued`,
//! `context`, `warming`, `start`, `first-token`, `delta` `{offset, text}`, then
//! `done` (the saved entry) or `error` `{message}`, plus a `ping` (null data)
//! at least every second while waiting. The sink returns `false` once its
//! consumer is gone; the answer is then stopped inside the helper (never by
//! killing it) and the ask finishes its bookkeeping.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use super::live_copilot::inputs_at;
use super::serve::Ask;
use crate::copilot::context::TimelineItem;
use crate::copilot::core::{ChatRow, Core, CoreConfig, Delta, Host, MessageStatus, Snapshot};
use crate::copilot::stream::Streamer;

/// How often a waiting ask checks that its consumer is still there.
const PING: Duration = Duration::from_secs(1);

enum HostEvent {
    State(Snapshot),
    Delta(Delta),
}

/// Copilot's host for one live ask: the live summary as of the cursor, the
/// run's sandbox settings, and its events sent to the ask's loop.
struct ReplayHost {
    timeline: Vec<TimelineItem>,
    sandbox: PathBuf,
    pool: sqlx::SqlitePool,
    model: Option<crate::copilot::CopilotModel>,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
}

#[async_trait]
impl Host for ReplayHost {
    fn emit_state(&self, snapshot: &Snapshot) {
        let _ = self.events.send(HostEvent::State(snapshot.clone()));
    }

    fn emit_delta(&self, delta: &Delta) {
        let _ = self.events.send(HostEvent::Delta(delta.clone()));
    }

    fn timeline(&self) -> Vec<TimelineItem> {
        self.timeline.clone()
    }

    fn suggestions_enabled(&self) -> bool {
        // Only the user's own question runs, never the detector.
        false
    }

    fn debug_enabled(&self) -> bool {
        true
    }

    fn is_paused(&self) -> bool {
        false
    }

    fn live_backlog_secs(&self) -> f64 {
        0.0
    }

    async fn streamer(&self, force_builtin: bool) -> Result<Arc<dyn Streamer>, String> {
        crate::copilot::resolve_streamer(
            &self.pool,
            Some(self.sandbox.clone()),
            self.model.clone(),
            force_builtin,
        )
        .await
    }

    async fn persist(&self, _rows: &[ChatRow]) -> Result<bool, String> {
        // Live asks go to the run's interactive.jsonl, written by the asker.
        Ok(true)
    }
}

/// The one Copilot request running, for Stop.
struct Current {
    run: String,
    core: Arc<Core>,
    id: String,
}

/// Live asks, one at a time: the helper serves one request anyway, and its
/// saved prefix is only reused when asks follow each other.
pub struct LiveAsker {
    turn: tokio::sync::Mutex<()>,
    current: Mutex<Option<Current>>,
    pools: tokio::sync::Mutex<HashMap<PathBuf, sqlx::SqlitePool>>,
}

impl Default for LiveAsker {
    fn default() -> Self {
        Self::new()
    }
}

/// Copilot's own timings, except a longer wait for the first word: a cold
/// prefix (the first ask, or one after the window moved) takes 14–17 s on a
/// 4B built-in model, and the viewer should show that answer and its latency
/// rather than the app's 15 s give-up. Stats still report the real latency.
pub fn live_config() -> CoreConfig {
    CoreConfig {
        first_token_timeout: Duration::from_secs(60),
        ..CoreConfig::default()
    }
}

/// The run's Copilot model setting, from its sandbox `store.json`.
fn sandbox_copilot_model(sandbox: &Path) -> Option<crate::copilot::CopilotModel> {
    let store: Value = std::fs::read_to_string(sandbox.join("store.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())?;
    crate::copilot::settings_from_stored(None, None, store.get(crate::copilot::MODEL_KEY)).model
}

fn status_name(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Pending => "pending",
        MessageStatus::Streaming => "streaming",
        MessageStatus::Done => "done",
        MessageStatus::Stopped => "stopped",
        MessageStatus::Error => "error",
    }
}

fn append_interactive(dir: &Path, entry: &Value) {
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("interactive.jsonl"))
        .and_then(|mut file| writeln!(file, "{entry}"));
    if let Err(error) = result {
        log::warn!("replay live copilot: could not save the live ask: {error}");
    }
}

impl LiveAsker {
    pub fn new() -> Self {
        Self {
            turn: tokio::sync::Mutex::new(()),
            current: Mutex::new(None),
            pools: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The run's sandbox database, opened read-only (its settings and keys).
    async fn pool(&self, sandbox: &Path) -> Result<sqlx::SqlitePool, String> {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(sandbox) {
            return Ok(pool.clone());
        }
        let file = sandbox.join("meeting_minutes.sqlite");
        if !file.is_file() {
            return Err(
                "This run has no sandbox database, so its model settings are unknown".into(),
            );
        }
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&file)
            .read_only(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .map_err(|e| format!("Could not open the run's sandbox database: {e}"))?;
        pools.insert(sandbox.to_path_buf(), pool.clone());
        Ok(pool)
    }

    /// Stops the running answer of `run`, if any.
    pub fn stop(&self, run: &str) -> bool {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        match current.as_ref() {
            Some(c) if c.run == run => {
                c.core.stop(&c.id);
                true
            }
            _ => false,
        }
    }

    /// Stops whatever answer is running.
    pub fn stop_any(&self) -> bool {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        match current.as_ref() {
            Some(c) => {
                c.core.stop(&c.id);
                true
            }
            None => false,
        }
    }

    /// Answers one live ask about run `run` (its folder `dir`), sending its
    /// progress to `sink`.
    pub async fn ask<S>(&self, run: &str, dir: &Path, ask: Ask, mut sink: S) -> Result<()>
    where
        S: FnMut(&str, Value) -> bool + Send,
    {
        let events = tokio::fs::read_to_string(dir.join("events.jsonl")).await?;
        let inputs = inputs_at(&events, ask.cutoff);
        drop(events);

        let mut gone = false;
        let mut send = |event: &str, data: Value, gone: &mut bool| {
            if !*gone && !sink(event, data) {
                *gone = true;
            }
        };

        let turn = match self.turn.try_lock() {
            Ok(turn) => turn,
            Err(_) => {
                send("queued", json!({}), &mut gone);
                self.turn.lock().await
            }
        };
        // Asked and left while queued: nothing to do.
        send("ping", Value::Null, &mut gone);
        if gone {
            return Ok(());
        }
        send(
            "context",
            json!({
                "lines": inputs.lines.len(),
                "timeline": inputs.timeline.len(),
                "notes_chars": inputs.notes.len(),
                "as_of_ms": inputs.latest_ms(),
            }),
            &mut gone,
        );

        let sandbox = dir.join("sandbox");
        let pool = match self.pool(&sandbox).await {
            Ok(pool) => pool,
            Err(error) => {
                send("error", json!({ "message": error }), &mut gone);
                return Ok(());
            }
        };
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let host = Arc::new(ReplayHost {
            timeline: inputs.timeline.clone(),
            sandbox: sandbox.clone(),
            pool,
            model: sandbox_copilot_model(&sandbox),
            events: sender,
        });

        // A cold built-in model loads first (tens of seconds for a 4B model),
        // so the first answer is not timed out by its load: Copilot's own
        // timeouts then cover the prompt and the answer, as in a meeting.
        let warm_started = Instant::now();
        let streamer = match host.streamer(false).await {
            Ok(streamer) => streamer,
            Err(error) => {
                send("error", json!({ "message": error }), &mut gone);
                return Ok(());
            }
        };
        if streamer.prompt_token_limit(1).is_some() {
            send(
                "warming",
                json!({ "provider": streamer.provider(), "model": streamer.model() }),
                &mut gone,
            );
            let tiny = crate::copilot::context::PromptParts {
                system: String::new(),
                stable: String::new(),
                anchor: 0,
                rest: "hi".into(),
            };
            let _ = streamer.count_tokens(&tiny).await;
        }
        let warm_ms = warm_started.elapsed().as_millis() as u64;
        drop(streamer);

        let core = Core::new(format!("replay-live-{run}"), host, live_config());
        for line in inputs.lines.iter().cloned() {
            core.on_line(line);
        }
        core.set_notes(inputs.notes.clone());
        let asked_at = chrono::Local::now().to_rfc3339();
        let started = Instant::now();
        let id = match core.ask(ask.action, ask.question.clone()) {
            Ok(id) => id,
            Err(error) => {
                send("error", json!({ "message": error }), &mut gone);
                return Ok(());
            }
        };
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(Current {
            run: run.to_string(),
            core: core.clone(),
            id: id.clone(),
        });

        let mut announced = false;
        let mut first_token_ms: Option<u64> = None;
        let mut finished = None;
        let mut last_write = Instant::now();
        let mut stop_sent = false;
        while finished.is_none() {
            match tokio::time::timeout(PING, receiver.recv()).await {
                Ok(Some(HostEvent::Delta(delta))) if delta.id == id => {
                    if first_token_ms.is_none() {
                        let ms = started.elapsed().as_millis() as u64;
                        first_token_ms = Some(ms);
                        send("first-token", json!({ "ms": ms }), &mut gone);
                    }
                    send(
                        "delta",
                        json!({ "offset": delta.offset, "text": delta.delta }),
                        &mut gone,
                    );
                    last_write = Instant::now();
                }
                Ok(Some(HostEvent::Delta(_))) => {}
                Ok(Some(HostEvent::State(snapshot))) => {
                    let Some(message) = snapshot.messages.into_iter().find(|m| m.id == id) else {
                        continue;
                    };
                    if !announced && message.status != MessageStatus::Pending {
                        announced = true;
                        send(
                            "start",
                            json!({
                                "id": id,
                                "provider": message.provider,
                                "model": message.model,
                                "as_of_ms": message.audio_ms,
                            }),
                            &mut gone,
                        );
                    }
                    if matches!(
                        message.status,
                        MessageStatus::Done | MessageStatus::Stopped | MessageStatus::Error
                    ) {
                        finished = Some(message);
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
            if !gone && last_write.elapsed() >= PING {
                // Also how a consumer that left is noticed.
                send("ping", Value::Null, &mut gone);
                last_write = Instant::now();
            }
            if gone && !stop_sent {
                stop_sent = true;
                core.stop(&id);
            }
        }
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
        core.deactivate();
        let Some(message) = finished else {
            send(
                "error",
                json!({ "message": "Copilot ended without an answer" }),
                &mut gone,
            );
            return Ok(());
        };
        let entry = json!({
            "id": id,
            "asked_at": asked_at,
            "audio_ms": ask.cutoff.audio_ms,
            "seq": ask.cutoff.seq,
            "as_of_ms": message.audio_ms,
            "kind": if ask.action.is_some() { "action" } else { "ask" },
            "action": message.action,
            "prompt": message.prompt,
            "reply": message.reply,
            "status": status_name(message.status),
            "error": message.error,
            "provider": message.provider,
            "model": message.model,
            "stats": message.stats,
            "server_first_token_ms": first_token_ms,
            "warm_ms": warm_ms,
            "context": {
                "lines": inputs.lines.len(),
                "timeline": inputs.timeline.len(),
                "notes_chars": inputs.notes.len(),
            },
        });
        append_interactive(dir, &entry);
        send("done", entry, &mut gone);
        drop(turn);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::serve::parse_ask;

    fn run_without_sandbox() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"seq\":0,\"t_ms\":0,\"audio_ms\":1000,\"type\":\"event\",\"name\":\"transcript-update\",\"payload\":{\"text\":\"Hello\",\"source\":\"system\",\"is_partial\":false,\"audio_start_time\":0.5,\"audio_end_time\":1.0}}\n",
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn a_run_without_a_sandbox_answers_with_an_error() {
        let dir = run_without_sandbox();
        let asker = LiveAsker::new();
        let ask = parse_ask(br#"{"audio_ms":5000,"kind":"ask","text":"What?"}"#).unwrap();
        let mut events: Vec<(String, Value)> = Vec::new();
        asker
            .ask("r", dir.path(), ask, |event, data| {
                events.push((event.to_string(), data));
                true
            })
            .await
            .unwrap();
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(names, ["ping", "context", "error"]);
        assert_eq!(events[1].1["lines"], 1);
        assert!(events[2].1["message"]
            .as_str()
            .unwrap()
            .contains("no sandbox database"));
        assert!(!dir.path().join("interactive.jsonl").exists());
        assert!(!asker.stop("r"), "nothing is left running");
    }

    #[tokio::test]
    async fn a_consumer_that_left_ends_the_ask() {
        let dir = run_without_sandbox();
        let asker = LiveAsker::new();
        let ask = parse_ask(br#"{"audio_ms":5000,"kind":"ask","text":"What?"}"#).unwrap();
        let mut calls = 0;
        asker
            .ask("r", dir.path(), ask, |_, _| {
                calls += 1;
                false
            })
            .await
            .unwrap();
        assert_eq!(calls, 1, "nothing is sent after the consumer is gone");
    }
}
