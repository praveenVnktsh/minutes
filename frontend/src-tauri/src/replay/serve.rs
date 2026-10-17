//! `replay serve`: the replay viewer, the local runs and a live Copilot, over
//! HTTP on 127.0.0.1 only.
//!
//! - `/replay/…` serves the viewer (embedded from `site/replay` at build time).
//! - `GET /api/runs` lists the runs under the replays folder, newest first.
//! - `GET /api/runs/<run>/events` and `/report` return a run's files;
//!   `/interactive` the questions asked live in the viewer.
//! - `GET /api/runs/<run>/tail?from=<line>` streams a run's events as
//!   server-sent events, following the file while the run is still writing it.
//! - `POST /api/runs/<run>/copilot` asks the real Copilot about the meeting as
//!   it stood at a cursor (see [`super::live_copilot`]) and streams the answer
//!   as server-sent events; `POST /api/runs/<run>/copilot/stop` stops it.
//!
//! Local and as read-only as it can be: it binds to the loopback address,
//! answers only requests addressed to it by that name (so a web page cannot
//! reach it through DNS rebinding), sends no CORS headers, refuses POSTs from
//! other origins or without a JSON body (so a page elsewhere cannot make the
//! browser send one), serves only files it embeds or the files of a run folder
//! directly under the replays root, and writes nothing but the run's
//! `interactive.jsonl`. The only network requests it makes are the Copilot
//! model's, to the provider the run's sandbox settings name (the built-in
//! model by default, which stays on this machine).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::live_copilot::{inputs_at, Cutoff};
use crate::copilot::context::TimelineItem;
use crate::copilot::core::{ChatRow, Core, CoreConfig, Delta, Host, MessageStatus, Snapshot};
use crate::copilot::prompt::QuickAction;
use crate::copilot::stream::Streamer;

pub const DEFAULT_PORT: u16 = 7357;

const ASSETS: &[(&str, &str, &str)] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_str!("../../../../site/replay/index.html"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/app.js"),
    ),
    (
        "parser.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/parser.js"),
    ),
    (
        "replay.css",
        "text/css; charset=utf-8",
        include_str!("../../../../site/replay/replay.css"),
    ),
    (
        "sample-data.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/sample-data.js"),
    ),
];

const FONTS: &[(&str, &[u8])] = &[
    (
        "source-sans-3-latin-variable.woff2",
        include_bytes!("../../../../site/fonts/source-sans-3-latin-variable.woff2"),
    ),
    (
        "source-serif-4-latin-variable.woff2",
        include_bytes!("../../../../site/fonts/source-serif-4-latin-variable.woff2"),
    ),
];

/// The most a POST body may be.
const MAX_BODY: usize = 64 * 1024;
/// The longest question accepted.
const MAX_QUESTION_CHARS: usize = 2_000;
/// How often an idle stream checks that the viewer is still there.
const PING: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Paths and hosts
// ---------------------------------------------------------------------------

/// A run folder name: what the recorder makes (`20261016-162607-m35`).
pub fn valid_run_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The folder of run `name`, only if it is a directory directly under `root`.
pub fn run_dir(root: &Path, name: &str) -> Option<PathBuf> {
    if !valid_run_name(name) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let dir = root.join(name).canonicalize().ok()?;
    (dir.parent() == Some(root.as_path()) && dir.is_dir()).then_some(dir)
}

/// Whether the `Host` header names this server (loopback, this port).
pub fn allowed_host(host: Option<&str>, port: u16) -> bool {
    matches!(host, Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

/// Whether a POST may change something: it comes from this server's own
/// page (or from no page at all, like curl) and carries JSON, which a
/// cross-origin page cannot send without a preflight this server never
/// answers.
pub fn allowed_post(origin: Option<&str>, content_type: Option<&str>, port: u16) -> bool {
    let same_origin = match origin {
        None => true,
        Some(o) => {
            o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
        }
    };
    let json = content_type
        .map(|c| {
            c.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false);
    same_origin && json
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Request {
    method: String,
    path: String,
    query: String,
    host: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    range: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut parts = first.split_whitespace();
    let mut request = Request {
        method: parts.next().unwrap_or_default().to_string(),
        ..Default::default()
    };
    let target = parts.next().unwrap_or_default().to_string();
    let mut length = 0usize;
    let mut total = first.len();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        total += read;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if total > 16 * 1024 {
            return Err(anyhow!("headers too large"));
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim().to_string();
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => request.host = Some(value),
                "origin" => request.origin = Some(value),
                "content-type" => request.content_type = Some(value),
                "range" => request.range = Some(value),
                "content-length" => length = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        return Err(anyhow!("body too large"));
    }
    if length > 0 {
        request.body = vec![0; length];
        reader.read_exact(&mut request.body)?;
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    request.path = path.to_string();
    request.query = query.to_string();
    Ok(request)
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn respond_json(stream: &mut TcpStream, status: &str, value: &Value) -> Result<()> {
    respond(
        stream,
        status,
        "application/json",
        &serde_json::to_vec(value)?,
    )
}

fn not_found(stream: &mut TcpStream) -> Result<()> {
    respond(stream, "404 Not Found", "text/plain", b"Not found")
}

fn start_event_stream(stream: &mut TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    )
}

/// One server-sent event. Every line of `data` gets its own `data:` field,
/// so the browser joins them back with newlines.
pub fn sse_frame(event: Option<&str>, data: &str) -> String {
    let mut frame = String::new();
    if let Some(event) = event {
        frame.push_str("event: ");
        frame.push_str(event);
        frame.push('\n');
    }
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line.strip_suffix('\r').unwrap_or(line));
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

fn send_event(stream: &mut TcpStream, event: &str, data: &Value) -> std::io::Result<()> {
    stream.write_all(sse_frame(Some(event), &data.to_string()).as_bytes())?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

fn first_line(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    // The header is in the first few records (after an activity update).
    let mut reader = BufReader::new(file);
    for _ in 0..8 {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let record: Value = serde_json::from_str(&line).ok()?;
        if record["name"] == "run-start" {
            return Some(record);
        }
    }
    None
}

/// The meeting folder's name: the recording is linked in the sandbox.
fn recording_name(dir: &Path) -> Option<String> {
    let meeting = dir.join("sandbox").join("meeting");
    std::fs::read_dir(meeting)
        .ok()?
        .flatten()
        .find_map(|entry| std::fs::read_link(entry.path()).ok())
        .and_then(|target| {
            target
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
        })
}

/// One run's entry in `/api/runs`.
fn run_summary(dir: &Path) -> Option<Value> {
    let name = dir.file_name()?.to_str()?.to_string();
    let events = dir.join("events.jsonl");
    let meta = events.metadata().ok()?;
    let header = first_line(&events)
        .map(|r| r["payload"].clone())
        .unwrap_or(Value::Null);
    let report: Option<Value> = std::fs::read_to_string(dir.join("report.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let fresh = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age < Duration::from_secs(60));
    let status = if report.is_some() {
        "done"
    } else if fresh {
        "running"
    } else {
        "incomplete"
    };
    let count = |status: &str| {
        report
            .as_ref()
            .and_then(|r| r["criteria"].as_array())
            .map(|c| c.iter().filter(|c| c["status"] == status).count())
    };
    let interactive = std::fs::read_to_string(dir.join("interactive.jsonl"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    Some(json!({
        "name": name,
        "status": status,
        "recording": recording_name(dir),
        "audio_name": header["audio_name"],
        "duration_s": header["duration_s"],
        "realtime": header["realtime"],
        "started_at": header["started_at"],
        "events_bytes": meta.len(),
        "passed": count("pass"),
        "failed": count("fail"),
        "interactive": interactive,
    }))
}

fn list_runs(root: &Path) -> Value {
    let mut runs: Vec<Value> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| e.file_name().to_str().is_some_and(valid_run_name))
        .filter_map(|e| run_summary(&e.path()))
        .collect();
    runs.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    json!({ "runs": runs })
}

fn is_end_record(line: &str) -> bool {
    line.contains("\"type\":\"meta\"")
        && (line.contains("\"name\":\"run-end\"") || line.contains("\"name\":\"run-error\""))
}

/// Streams `events.jsonl` from line `from` as server-sent events, following
/// the file until the run ends (or the viewer goes away).
fn tail(stream: &mut TcpStream, dir: &Path, from: usize) -> Result<()> {
    start_event_stream(stream)?;
    let path = dir.join("events.jsonl");
    let mut offset = 0u64;
    let mut pending = String::new();
    let mut line_no = 0usize;
    let mut last_send = Instant::now();
    loop {
        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = Vec::new();
        let read = file.read_to_end(&mut chunk)?;
        offset += read as u64;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        let mut ended = false;
        let mut out = String::new();
        while let Some(newline) = pending.find('\n') {
            let line: String = pending.drain(..=newline).collect();
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            if line_no >= from {
                out.push_str(&sse_frame(None, line));
            }
            line_no += 1;
            ended |= is_end_record(line);
        }
        if !out.is_empty() {
            stream.write_all(out.as_bytes())?;
            last_send = Instant::now();
        }
        let finished = dir.join("report.json").exists() && read == 0;
        if ended || finished {
            stream.write_all(sse_frame(Some("end"), "{}").as_bytes())?;
            return Ok(());
        }
        if last_send.elapsed() > Duration::from_secs(15) {
            stream.write_all(b": keep-alive\n\n")?;
            last_send = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn query_number(query: &str, key: &str) -> usize {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The recording
// ---------------------------------------------------------------------------

/// Audio files a run may link, and their types.
const AUDIO_TYPES: &[(&str, &str)] = &[
    ("mp4", "audio/mp4"),
    ("m4a", "audio/mp4"),
    ("wav", "audio/wav"),
    ("mp3", "audio/mpeg"),
    ("webm", "audio/webm"),
    ("ogg", "audio/ogg"),
];

/// The recording a run replayed: the `audio.<ext>` the recorder linked into
/// its sandbox's meeting folder, and nothing else. `Err` names the missing
/// file when the link no longer resolves.
pub fn run_audio(dir: &Path) -> Result<(PathBuf, &'static str), String> {
    let meeting = dir.join("sandbox").join("meeting");
    let entries =
        std::fs::read_dir(&meeting).map_err(|_| "This run has no linked recording".to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(ext) = name.strip_prefix("audio.") else {
            continue;
        };
        let Some((_, kind)) = AUDIO_TYPES
            .iter()
            .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        else {
            continue;
        };
        let link = entry.path();
        return match link.canonicalize() {
            Ok(file) if file.is_file() => Ok((file, kind)),
            _ => Err(format!(
                "The recording is not on this machine any more: {}",
                std::fs::read_link(&link).unwrap_or(link).display()
            )),
        };
    }
    Err("This run has no linked recording".to_string())
}

/// A `Range` header against a body of `len` bytes: `Ok(None)` for the whole
/// body (no header, or one this server does not honour, such as several
/// ranges), `Ok(Some((first, last)))` inclusive, `Err(())` when unsatisfiable
/// (416).
pub fn parse_range(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Ok(None);
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // The last N bytes.
        let Ok(n) = last.parse::<u64>() else {
            return Ok(None);
        };
        if n == 0 || len == 0 {
            return Err(());
        }
        return Ok(Some((len.saturating_sub(n), len - 1)));
    }
    let Ok(first) = first.parse::<u64>() else {
        return Ok(None);
    };
    let last = if last.is_empty() {
        u64::MAX
    } else {
        match last.parse::<u64>() {
            Ok(last) if last >= first => last,
            _ => return Ok(None),
        }
    };
    if first >= len {
        return Err(());
    }
    Ok(Some((first, last.min(len - 1))))
}

/// Streams the run's recording, honouring `Range` so the browser can seek.
fn serve_audio(stream: &mut TcpStream, dir: &Path, range: Option<&str>) -> Result<()> {
    let (path, kind) = match run_audio(dir) {
        Ok(found) => found,
        Err(message) => {
            return respond(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                message.as_bytes(),
            )
        }
    };
    let mut file = std::fs::File::open(&path)?;
    let len = file.metadata()?.len();
    let head = |status: &str, length: u64, extra: String| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {length}\r\n\
             Accept-Ranges: bytes\r\n{extra}Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
             Connection: close\r\n\r\n"
        )
    };
    match parse_range(range, len) {
        Err(()) => {
            write!(
                stream,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\n\
                 Accept-Ranges: bytes\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
        }
        Ok(None) => {
            stream.write_all(head("200 OK", len, String::new()).as_bytes())?;
            std::io::copy(&mut file, stream)?;
        }
        Ok(Some((first, last))) => {
            let length = last - first + 1;
            stream.write_all(
                head(
                    "206 Partial Content",
                    length,
                    format!("Content-Range: bytes {first}-{last}/{len}\r\n"),
                )
                .as_bytes(),
            )?;
            file.seek(SeekFrom::Start(first))?;
            std::io::copy(&mut file.take(length), stream)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Live Copilot
// ---------------------------------------------------------------------------

/// `POST /api/runs/<run>/copilot`.
#[derive(Debug, Deserialize)]
pub struct AskBody {
    /// Meeting time of the cursor.
    pub audio_ms: u64,
    /// The cursor as a record count (exact); `audio_ms` alone otherwise.
    #[serde(default)]
    pub seq: Option<u64>,
    /// `ask` (a question) or `action` (a quick action).
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

/// A checked ask: the quick action or the question.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub cutoff: Cutoff,
    pub action: Option<QuickAction>,
    pub question: Option<String>,
}

pub fn parse_ask(body: &[u8]) -> Result<Ask, String> {
    let body: AskBody =
        serde_json::from_slice(body).map_err(|e| format!("Bad request body: {e}"))?;
    let cutoff = Cutoff {
        audio_ms: body.audio_ms,
        seq: body.seq,
    };
    match body.kind.as_str() {
        "ask" => {
            let text = body.text.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return Err("Type a question first".into());
            }
            if text.chars().count() > MAX_QUESTION_CHARS {
                return Err(format!(
                    "Questions are limited to {MAX_QUESTION_CHARS} characters"
                ));
            }
            Ok(Ask {
                cutoff,
                action: None,
                question: Some(text),
            })
        }
        "action" => {
            let action = body
                .action
                .as_deref()
                .and_then(QuickAction::parse)
                .filter(|a| *a != QuickAction::Shorter)
                .ok_or("Unknown quick action")?;
            Ok(Ask {
                cutoff,
                action: Some(action),
                question: None,
            })
        }
        _ => Err("kind must be \"ask\" or \"action\"".into()),
    }
}

enum HostEvent {
    State(Snapshot),
    Delta(Delta),
}

/// Copilot's host for one live ask: the live summary as of the cursor, the
/// run's sandbox settings, and its events sent to the viewer's stream.
struct ReplayHost {
    timeline: Vec<TimelineItem>,
    sandbox: PathBuf,
    pool: sqlx::SqlitePool,
    model: Option<crate::copilot::CopilotModel>,
    events: Mutex<mpsc::Sender<HostEvent>>,
}

#[async_trait]
impl Host for ReplayHost {
    fn emit_state(&self, snapshot: &Snapshot) {
        let _ = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(HostEvent::State(snapshot.clone()));
    }

    fn emit_delta(&self, delta: &Delta) {
        let _ = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(HostEvent::Delta(delta.clone()));
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
        // Live asks go to the run's interactive.jsonl, written by the server.
        Ok(true)
    }
}

/// The one Copilot request running, for Stop.
struct Current {
    run: String,
    core: Arc<Core>,
    id: String,
}

/// State shared by the server's connections for live Copilot asks.
struct Live {
    runtime: tokio::runtime::Runtime,
    /// One ask at a time: the helper serves one request anyway, and its
    /// saved prefix is only reused when asks follow each other.
    turn: tokio::sync::Mutex<()>,
    current: Mutex<Option<Current>>,
    pools: tokio::sync::Mutex<HashMap<PathBuf, sqlx::SqlitePool>>,
}

impl Live {
    fn new() -> Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?,
            turn: tokio::sync::Mutex::new(()),
            current: Mutex::new(None),
            pools: tokio::sync::Mutex::new(HashMap::new()),
        })
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

    fn stop(&self, run: &str) -> bool {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        match current.as_ref() {
            Some(c) if c.run == run => {
                c.core.stop(&c.id);
                true
            }
            _ => false,
        }
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
        log::warn!("replay serve: could not save the live ask: {error}");
    }
}

/// Streams one live Copilot answer. Events: `queued`, `context`, `warming`,
/// `start`, `delta`, `first-token`, `done` (with the saved entry) or `error`.
fn copilot(stream: &mut TcpStream, live: &Live, run: &str, dir: &Path, body: &[u8]) -> Result<()> {
    let ask = match parse_ask(body) {
        Ok(ask) => ask,
        Err(error) => return respond_json(stream, "400 Bad Request", &json!({ "error": error })),
    };
    let events = std::fs::read_to_string(dir.join("events.jsonl"))?;
    let inputs = inputs_at(&events, ask.cutoff);
    drop(events);
    start_event_stream(stream)?;
    let mut gone = false;
    let send = |stream: &mut TcpStream, event: &str, data: Value, gone: &mut bool| {
        if !*gone && send_event(stream, event, &data).is_err() {
            *gone = true;
        }
    };

    let turn = match live.turn.try_lock() {
        Ok(turn) => turn,
        Err(_) => {
            send(stream, "queued", json!({}), &mut gone);
            live.runtime.block_on(live.turn.lock())
        }
    };
    // Asked and left while queued: nothing to do.
    if gone || stream.write_all(b": ping\n\n").is_err() {
        return Ok(());
    }
    send(
        stream,
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
    let pool = match live.runtime.block_on(live.pool(&sandbox)) {
        Ok(pool) => pool,
        Err(error) => {
            send(stream, "error", json!({ "message": error }), &mut gone);
            return Ok(());
        }
    };
    let (sender, receiver) = mpsc::channel();
    let host = Arc::new(ReplayHost {
        timeline: inputs.timeline.clone(),
        sandbox: sandbox.clone(),
        pool,
        model: sandbox_copilot_model(&sandbox),
        events: Mutex::new(sender),
    });
    let _runtime = live.runtime.enter();

    // A cold built-in model loads first (tens of seconds for a 4B model), so
    // the first answer is not timed out by its load: Copilot's own timeouts
    // then cover the prompt and the answer, as they do in a meeting.
    let warm_started = Instant::now();
    let streamer = match live.runtime.block_on(host.streamer(false)) {
        Ok(streamer) => streamer,
        Err(error) => {
            send(stream, "error", json!({ "message": error }), &mut gone);
            return Ok(());
        }
    };
    if streamer.prompt_token_limit(1).is_some() {
        send(
            stream,
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
        let _ = live.runtime.block_on(streamer.count_tokens(&tiny));
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
            send(stream, "error", json!({ "message": error }), &mut gone);
            return Ok(());
        }
    };
    *live.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(Current {
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
        match receiver.recv_timeout(PING) {
            Ok(HostEvent::Delta(delta)) if delta.id == id => {
                if first_token_ms.is_none() {
                    let ms = started.elapsed().as_millis() as u64;
                    first_token_ms = Some(ms);
                    send(stream, "first-token", json!({ "ms": ms }), &mut gone);
                }
                send(
                    stream,
                    "delta",
                    json!({ "offset": delta.offset, "text": delta.delta }),
                    &mut gone,
                );
                last_write = Instant::now();
            }
            Ok(HostEvent::Delta(_)) => {}
            Ok(HostEvent::State(snapshot)) => {
                let Some(message) = snapshot.messages.into_iter().find(|m| m.id == id) else {
                    continue;
                };
                if !announced && message.status != MessageStatus::Pending {
                    announced = true;
                    send(
                        stream,
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
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !gone && last_write.elapsed() >= PING {
            // Also how a closed tab is noticed: the write fails.
            if stream.write_all(b": ping\n\n").is_err() {
                gone = true;
            }
            last_write = Instant::now();
        }
        if gone && !stop_sent {
            stop_sent = true;
            // The viewer left: stop the answer inside the helper (never by
            // killing it) and let the request finish its bookkeeping.
            core.stop(&id);
        }
    }
    *live.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
    core.deactivate();
    let Some(message) = finished else {
        send(
            stream,
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
    send(stream, "done", entry, &mut gone);
    drop(turn);
    Ok(())
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

#[cfg(test)]
fn handle(stream: TcpStream, root: &Path, port: u16, live: &Live) -> Result<()> {
    handle_with(stream, root, port, live, None)
}

fn handle_with(
    mut stream: TcpStream,
    root: &Path,
    port: u16,
    live: &Live,
    site: Option<&Path>,
) -> Result<()> {
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(_) => return respond(&mut stream, "400 Bad Request", "text/plain", b"Bad request"),
    };
    if !allowed_host(request.host.as_deref(), port) {
        return respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden");
    }
    let path = request.path.as_str();
    if request.method == "POST" {
        if !allowed_post(
            request.origin.as_deref(),
            request.content_type.as_deref(),
            port,
        ) {
            return respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden");
        }
        let Some(rest) = path.strip_prefix("/api/runs/") else {
            return not_found(&mut stream);
        };
        let Some((name, action)) = rest.split_once('/') else {
            return not_found(&mut stream);
        };
        let Some(dir) = run_dir(root, name) else {
            return not_found(&mut stream);
        };
        return match action {
            "copilot" => copilot(&mut stream, live, name, &dir, &request.body),
            "copilot/stop" => {
                let stopped = live.stop(name);
                respond_json(&mut stream, "200 OK", &json!({ "stopped": stopped }))
            }
            _ => not_found(&mut stream),
        };
    }
    if request.method != "GET" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"Method not allowed",
        );
    }
    if path == "/" || path == "/replay" {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: /replay/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    if let Some(asset) = path.strip_prefix("/replay/") {
        let asset = if asset.is_empty() {
            "index.html"
        } else {
            asset
        };
        return match ASSETS.iter().find(|(name, _, _)| *name == asset) {
            // `--site`: the viewer's files from disk, for working on it.
            Some((name, kind, _)) if site.is_some() => {
                match std::fs::read(site.unwrap_or(root).join(name)) {
                    Ok(body) => respond(&mut stream, "200 OK", kind, &body),
                    Err(_) => not_found(&mut stream),
                }
            }
            Some((_, kind, body)) => respond(&mut stream, "200 OK", kind, body.as_bytes()),
            None => not_found(&mut stream),
        };
    }
    if let Some(font) = path.strip_prefix("/fonts/") {
        return match FONTS.iter().find(|(name, _)| *name == font) {
            Some((_, body)) => respond(&mut stream, "200 OK", "font/woff2", body),
            None => not_found(&mut stream),
        };
    }
    if path == "/api/runs" {
        return respond_json(&mut stream, "200 OK", &list_runs(root));
    }
    if let Some(rest) = path.strip_prefix("/api/runs/") {
        let Some((name, what)) = rest.split_once('/') else {
            return not_found(&mut stream);
        };
        let Some(dir) = run_dir(root, name) else {
            return not_found(&mut stream);
        };
        let file =
            |name: &str, kind: &str, stream: &mut TcpStream| match std::fs::read(dir.join(name)) {
                Ok(body) => respond(stream, "200 OK", kind, &body),
                Err(_) => not_found(stream),
            };
        return match what {
            "events" => file("events.jsonl", "application/x-ndjson", &mut stream),
            "report" => file("report.json", "application/json", &mut stream),
            "audio" => serve_audio(&mut stream, &dir, request.range.as_deref()),
            "interactive" => match std::fs::read(dir.join("interactive.jsonl")) {
                Ok(body) => respond(&mut stream, "200 OK", "application/x-ndjson", &body),
                Err(_) => respond(&mut stream, "200 OK", "application/x-ndjson", b""),
            },
            "tail" => tail(&mut stream, &dir, query_number(&request.query, "from")),
            _ => not_found(&mut stream),
        };
    }
    not_found(&mut stream)
}

/// Binds the server's socket: the loopback address only.
pub fn bind(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| anyhow!("could not listen on 127.0.0.1:{port}: {e}"))
}

/// Serves the viewer and the runs under `root` until killed. `site` serves
/// the viewer from that folder instead of the copy built in.
pub fn serve(root: PathBuf, port: u16, open: bool, site: Option<PathBuf>) -> Result<()> {
    std::fs::create_dir_all(&root)?;
    let listener = bind(port)?;
    let live = Arc::new(Live::new()?);
    let url = format!("http://127.0.0.1:{port}/replay/");
    println!("Replay viewer: {url}");
    println!(
        "Runs: {} (this machine only; live Copilot asks are saved to each run's interactive.jsonl; Ctrl-C to stop)",
        root.display()
    );
    if open {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let root = root.clone();
        let live = live.clone();
        let site = site.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle_with(stream, &root, port, &live, site.as_deref()) {
                log::debug!("replay serve: {error:#}");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_names_and_folders_stay_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("20261016-162607-m35")).unwrap();
        std::fs::write(root.path().join("file.txt"), "x").unwrap();
        assert!(run_dir(root.path(), "20261016-162607-m35").is_some());
        for bad in [
            "", "..", "../etc", "a/b", ".hidden", "file.txt", "missing", "%2e%2e", "..%2f",
        ] {
            assert!(run_dir(root.path(), bad).is_none(), "{bad}");
        }
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
            assert!(
                run_dir(root.path(), "link").is_none(),
                "a symlink out of the root"
            );
        }
    }

    #[test]
    fn only_this_server_s_host_is_answered() {
        assert!(allowed_host(Some("127.0.0.1:7357"), 7357));
        assert!(allowed_host(Some("localhost:7357"), 7357));
        assert!(!allowed_host(Some("evil.example:7357"), 7357));
        assert!(!allowed_host(Some("127.0.0.1:80"), 7357));
        assert!(!allowed_host(None, 7357));
    }

    #[test]
    fn posts_must_be_json_from_this_page() {
        assert!(allowed_post(
            Some("http://127.0.0.1:7357"),
            Some("application/json"),
            7357
        ));
        assert!(allowed_post(
            None,
            Some("application/json; charset=utf-8"),
            7357
        ));
        assert!(!allowed_post(
            Some("https://evil.example"),
            Some("application/json"),
            7357
        ));
        assert!(!allowed_post(
            Some("http://127.0.0.1:7357"),
            Some("text/plain"),
            7357
        ));
        assert!(!allowed_post(None, None, 7357));
    }

    #[test]
    fn the_server_listens_on_loopback_only() {
        let listener = bind(0).unwrap();
        let address = listener.local_addr().unwrap();
        assert!(address.ip().is_loopback(), "{address}");
    }

    #[test]
    fn requests_from_other_hosts_are_refused_over_http() {
        let root = tempfile::tempdir().unwrap();
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let live = Arc::new(Live::new().unwrap());
        let root_path = root.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let _ = handle(stream.unwrap(), &root_path, port, &live);
            }
        });
        let get = |request: String| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let ok = get(format!(
            "GET /api/runs HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        ));
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(!ok.to_ascii_lowercase().contains("access-control-allow"));
        let rebound = get("GET /api/runs HTTP/1.1\r\nHost: evil.example\r\n\r\n".into());
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
        let traversal = get(format!(
            "GET /api/runs/../events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        ));
        assert!(traversal.starts_with("HTTP/1.1 404"), "{traversal}");
    }

    #[test]
    fn runs_are_listed_with_their_status() {
        let root = tempfile::tempdir().unwrap();
        let done = root.path().join("20261016-100000-a");
        std::fs::create_dir(&done).unwrap();
        std::fs::write(
            done.join("events.jsonl"),
            "{\"seq\":0,\"t_ms\":0,\"audio_ms\":0,\"type\":\"event\",\"name\":\"meeting-activity-updated\",\"payload\":{}}\n\
             {\"seq\":1,\"t_ms\":0,\"audio_ms\":0,\"type\":\"meta\",\"name\":\"run-start\",\"payload\":{\"audio_name\":\"audio.mp4\",\"duration_s\":60}}\n",
        )
        .unwrap();
        std::fs::write(
            done.join("report.json"),
            "{\"criteria\":[{\"status\":\"pass\"},{\"status\":\"fail\"},{\"status\":\"pass\"}]}",
        )
        .unwrap();
        std::fs::write(done.join("interactive.jsonl"), "{}\n{}\n").unwrap();
        let running = root.path().join("20261016-110000-b");
        std::fs::create_dir(&running).unwrap();
        std::fs::write(running.join("events.jsonl"), "").unwrap();
        std::fs::create_dir(root.path().join("no-events")).unwrap();
        let list = list_runs(root.path());
        let runs = list["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["name"], "20261016-110000-b");
        assert_eq!(runs[0]["status"], "running");
        assert_eq!(runs[1]["status"], "done");
        assert_eq!(runs[1]["passed"], 2);
        assert_eq!(runs[1]["failed"], 1);
        assert_eq!(runs[1]["duration_s"], 60);
        assert_eq!(runs[1]["interactive"], 2);
    }

    #[test]
    fn end_records_end_the_live_stream() {
        assert!(is_end_record(
            "{\"seq\":9,\"t_ms\":1,\"audio_ms\":1,\"type\":\"meta\",\"name\":\"run-end\",\"payload\":{}}"
        ));
        assert!(!is_end_record(
            "{\"type\":\"debug\",\"name\":\"phase\",\"payload\":{\"phase\":\"run-end\"}}"
        ));
        assert_eq!(query_number("from=120&x=1", "from"), 120);
        assert_eq!(query_number("", "from"), 0);
    }

    #[test]
    fn server_sent_events_are_framed_line_by_line() {
        assert_eq!(
            sse_frame(Some("delta"), "{\"text\":\"hi\"}"),
            "event: delta\ndata: {\"text\":\"hi\"}\n\n"
        );
        assert_eq!(
            sse_frame(None, "a\nb\r\nc"),
            "data: a\ndata: b\ndata: c\n\n"
        );
        // JSON never carries a raw newline, so one record is one data line.
        let data = json!({ "text": "two\nlines" }).to_string();
        let frame = sse_frame(Some("delta"), &data);
        assert_eq!(frame.matches("data: ").count(), 1);
        assert!(frame.ends_with("\n\n"));
    }

    #[test]
    fn asks_are_checked() {
        let ask = parse_ask(br#"{"audio_ms":720000,"seq":3000,"kind":"ask","text":"  What are they asking me for?  "}"#).unwrap();
        assert_eq!(
            ask.question.as_deref(),
            Some("What are they asking me for?")
        );
        assert_eq!(
            ask.cutoff,
            Cutoff {
                audio_ms: 720_000,
                seq: Some(3000)
            }
        );
        let action = parse_ask(br#"{"audio_ms":1,"kind":"action","action":"catch_up"}"#).unwrap();
        assert_eq!(action.action, Some(QuickAction::CatchUp));
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"ask","text":"  "}"#).is_err());
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"action","action":"shorter"}"#).is_err());
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"action","action":"nope"}"#).is_err());
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"other"}"#).is_err());
        let long = format!(
            r#"{{"audio_ms":1,"kind":"ask","text":"{}"}}"#,
            "a".repeat(2_001)
        );
        assert!(parse_ask(long.as_bytes()).is_err());
    }

    #[test]
    fn ranges_are_parsed_like_http_says() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-0"), 100), Ok(Some((0, 0))));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Ok(Some((10, 19))));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Ok(Some((90, 99))));
        assert_eq!(
            parse_range(Some("bytes=90-500"), 100),
            Ok(Some((90, 99))),
            "clamped"
        );
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            Ok(Some((90, 99))),
            "suffix"
        );
        assert_eq!(
            parse_range(Some("bytes=-500"), 100),
            Ok(Some((0, 99))),
            "suffix over the length"
        );
        assert_eq!(
            parse_range(Some("bytes=100-"), 100),
            Err(()),
            "past the end"
        );
        assert_eq!(parse_range(Some("bytes=-0"), 100), Err(()));
        assert_eq!(parse_range(Some("bytes=0-"), 0), Err(()), "an empty file");
        assert_eq!(
            parse_range(Some("bytes=5-2"), 100),
            Ok(None),
            "invalid: ignored"
        );
        assert_eq!(
            parse_range(Some("bytes=0-1,5-6"), 100),
            Ok(None),
            "several: whole body"
        );
        assert_eq!(parse_range(Some("items=0-1"), 100), Ok(None));
    }

    #[test]
    fn only_the_run_s_linked_recording_is_served_with_ranges() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20261016-100000-a");
        let meeting = run.join("sandbox").join("meeting");
        std::fs::create_dir_all(&meeting).unwrap();
        let recording = tempfile::tempdir().unwrap();
        let source = recording.path().join("audio.m4a");
        std::fs::write(&source, (0u8..100).collect::<Vec<u8>>()).unwrap();
        std::fs::write(meeting.join("other.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, meeting.join("audio.m4a")).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&source, meeting.join("audio.m4a")).unwrap();
        let (path, kind) = run_audio(&run).unwrap();
        assert_eq!(path, source.canonicalize().unwrap());
        assert_eq!(kind, "audio/mp4");

        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let live = Arc::new(Live::new().unwrap());
        let root_path = root.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let _ = handle(stream.unwrap(), &root_path, port, &live);
            }
        });
        let get = |range: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "GET /api/runs/20261016-100000-a/audio HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{range}\r\n"
            )
            .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        };
        let whole = get("");
        assert!(whole.starts_with(b"HTTP/1.1 200"));
        assert!(String::from_utf8_lossy(&whole).contains("Accept-Ranges: bytes"));
        let part = get("Range: bytes=10-14\r\n");
        let text = String::from_utf8_lossy(&part);
        assert!(text.starts_with("HTTP/1.1 206"), "{text}");
        assert!(text.contains("Content-Range: bytes 10-14/100"));
        assert!(text.contains("Content-Type: audio/mp4"));
        assert!(part.ends_with(&[10, 11, 12, 13, 14]));
        let tail = get("Range: bytes=-3\r\n");
        assert!(tail.ends_with(&[97, 98, 99]));
        let past = String::from_utf8_lossy(&get("Range: bytes=200-\r\n")).to_string();
        assert!(past.starts_with("HTTP/1.1 416"), "{past}");
        assert!(past.contains("Content-Range: bytes */100"));

        std::fs::remove_file(&source).unwrap();
        assert!(run_audio(&run).unwrap_err().contains("not on this machine"));
    }
}
