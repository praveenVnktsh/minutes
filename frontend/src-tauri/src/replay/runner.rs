//! Runs a recorded meeting through the real live pipeline, headless.
//!
//! The app is built on Tauri's mock runtime with its own app-data folder (a
//! sandbox under the real one's `replays/<run>/sandbox`), so the replay never
//! reads or writes the user's meetings database and never adds a visible
//! meeting. Settings are copied in from the real database (read-only) and
//! `store.json`; models are linked, file by file, so a model the replay would
//! delete or download only affects the sandbox.
//!
//! The audio is decoded with ffmpeg and fed as the pipeline would (600 ms
//! windows per source, the recording as the system ("Others") source and
//! silence as the microphone), as fast as live transcription decodes it.
//! Recording-time rules run on `crate::clock`, which the replay simulates:
//! meeting time jumps ahead with the audio while no model work is in flight,
//! and runs at the wall clock's rate while it is, so a request taking 1.2 s
//! still lets 1.2 s of audio arrive meanwhile. `realtime` feeds at real speed
//! instead, for occasional fidelity runs. Everything the UI
//! would hear is written to `events.jsonl` with debug measurements alongside;
//! see [`super::events`].

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::{json, Value};
use sqlx::Row;
use tauri::test::MockRuntime;
use tauri::{AppHandle, Listener, Manager};
use tokio::sync::mpsc::UnboundedSender;

use super::events::{debug, Record, RecordType, SCHEMA_VERSION, UI_EVENTS};
use super::report;
use super::script::{self, Scheduled, StepKind, TopicEntry};
use crate::audio::{AudioChunk, RecordingDeviceType};
use crate::database::manager::DatabaseManager;
use crate::state::AppState;

/// The real app's identifier: its data folder is the one settings and models
/// are read from, and replays are written under.
pub const APP_IDENTIFIER: &str = "com.meetily.ai";
const SAMPLE_RATE: u32 = 48_000;
/// The pipeline's mixing window.
const WINDOW_MS: u64 = 600;
const WINDOW_SAMPLES: usize = (SAMPLE_RATE as u64 * WINDOW_MS / 1000) as usize;
/// How long the stop path lets streaming transcription drain.
const DRAIN_LIMIT: Duration = Duration::from_secs(30);

pub struct Options {
    pub audio: PathBuf,
    pub script: Option<PathBuf>,
    /// `live-notes.json` to feed as the user's notes; `None` and
    /// `use_notes` looks next to the audio.
    pub notes: Option<PathBuf>,
    pub use_notes: bool,
    /// Feed at real speed instead of as fast as decoding allows.
    pub realtime: bool,
    pub verbose: bool,
    pub summary_runs: u32,
    pub skip_final: bool,
    /// Replay only the first N seconds (smoke tests).
    pub limit_seconds: Option<f64>,
    pub label: Option<String>,
}

// ---------------------------------------------------------------------------
// Event log
// ---------------------------------------------------------------------------

struct Log {
    seq: u64,
    file: BufWriter<File>,
    records: Vec<Record>,
}

pub struct Recorder {
    started: Instant,
    audio_ms: AtomicU64,
    log: Mutex<Log>,
}

impl Recorder {
    fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            started: Instant::now(),
            audio_ms: AtomicU64::new(0),
            log: Mutex::new(Log {
                seq: 0,
                file: BufWriter::new(File::create(path)?),
                records: Vec::new(),
            }),
        })
    }

    fn t_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn write(&self, kind: RecordType, name: &str, payload: Value) {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        let record = Record {
            seq: log.seq,
            t_ms: self.t_ms(),
            audio_ms: meeting_ms(),
            kind,
            name: name.to_string(),
            payload,
        };
        log.seq += 1;
        let line = record.to_line();
        let _ = writeln!(log.file, "{line}");
        log.records.push(record);
    }

    fn debug(&self, name: &str, payload: Value) {
        self.write(RecordType::Debug, name, payload);
    }

    fn phase(&self, phase: &str, extra: Value) {
        let mut payload = json!({ "phase": phase });
        if let (Some(target), Value::Object(extra)) = (payload.as_object_mut(), extra) {
            target.extend(extra);
        }
        println!("[replay {:>7.1}s] {phase}", self.t_ms() as f64 / 1000.0);
        self.debug(debug::PHASE, payload);
    }

    fn flush(&self) {
        let _ = self
            .log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .file
            .flush();
    }

    fn records(&self) -> Vec<Record> {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .clone()
    }
}

/// Meeting time: the simulated clock the recording-time features run on.
fn meeting_ms() -> u64 {
    crate::clock::simulation::elapsed().as_millis() as u64
}

// ---------------------------------------------------------------------------
// Logging: the app's log to a file, sidecar activity and warnings as events
// ---------------------------------------------------------------------------

static LOG_RECORDER: OnceLock<Arc<Recorder>> = OnceLock::new();

struct ReplayLogger {
    file: Mutex<BufWriter<File>>,
    /// The last warning recorded as an event, and how many repeats of it
    /// were folded since (a warning a hot loop repeats would flood the log).
    last_warning: Mutex<(String, u64, u64)>,
}

/// Repeats of one warning within this long become a single event.
const WARNING_REPEAT_MS: u64 = 5_000;

/// The part of a warning that identifies it: its target and wording without
/// numbers.
fn warning_key(target: &str, message: &str) -> String {
    let words: String = message
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .take(60)
        .collect();
    format!("{target}|{words}")
}

/// What a log line says about llama-helper, for the sidecar lane.
pub fn classify_sidecar(message: &str) -> Option<&'static str> {
    const PATTERNS: &[(&str, &str)] = &[
        ("Pre-empting background llama-helper request", "preempt"),
        ("cancelling request", "cancel"),
        ("Copilot needed the model", "timeline-preempted"),
        ("Spawning llama-helper sidecar", "spawn"),
        ("Sidecar spawned successfully", "spawned"),
        ("Sending generation request to sidecar", "request"),
        ("gave no reply within", "timeout"),
        ("shutting down the sidecar", "stop"),
        ("Sidecar exited", "exit"),
        ("idle sidecar", "idle-exit"),
        ("Switching llama-helper model", "model-switch"),
        ("switching model", "model-switch"),
    ];
    PATTERNS
        .iter()
        .find(|(pattern, _)| message.contains(pattern))
        .map(|(_, what)| *what)
}

impl log::Log for ReplayLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let recorder = LOG_RECORDER.get();
        if let Ok(mut file) = self.file.lock() {
            let t = recorder.map(|r| r.t_ms()).unwrap_or(0);
            let _ = writeln!(
                file,
                "{:>9.3} {:<5} {}: {}",
                t as f64 / 1000.0,
                record.level(),
                record.target(),
                message
            );
        }
        let Some(recorder) = recorder else {
            return;
        };
        if let Some(what) = classify_sidecar(&message) {
            recorder.debug(
                debug::SIDECAR,
                json!({ "what": what, "message": truncate(&message, 300) }),
            );
        }
        if record.level() <= log::Level::Warn {
            let key = warning_key(record.target(), &message);
            let now = recorder.t_ms();
            let repeats = {
                let mut last = self.last_warning.lock().unwrap_or_else(|e| e.into_inner());
                if last.0 == key && now.saturating_sub(last.1) < WARNING_REPEAT_MS {
                    last.2 += 1;
                    return;
                }
                let repeats = last.2;
                *last = (key, now, 0);
                repeats
            };
            recorder.debug(
                debug::LOG,
                json!({
                    "level": record.level().to_string(),
                    "target": record.target(),
                    "message": truncate(&message, 300),
                    "folded_repeats_of_previous": repeats,
                }),
            );
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "…"
    }
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

fn real_data_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow!("No data folder on this system"))?
        .join(APP_IDENTIFIER))
}

/// Mirror `from` into `to`: directories are created, files are symlinked.
fn link_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else {
            std::os::unix::fs::symlink(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copies the user's summary and transcription settings into the sandbox
/// database. The real database is opened read-only.
async fn copy_settings(real_db: &Path, pool: &sqlx::SqlitePool) -> Result<()> {
    if !real_db.exists() {
        println!("No app database found; the sandbox uses default settings");
        return Ok(());
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(real_db)
        .read_only(true);
    let real = sqlx::SqlitePool::connect_with(options).await?;
    for table in ["settings", "transcript_settings"] {
        let columns: Vec<String> = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await?
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect();
        let list = columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = sqlx::query(&format!("SELECT {list} FROM {table}"))
            .fetch_all(&real)
            .await
            .with_context(|| format!("reading {table}"))?;
        let marks = vec!["?"; columns.len()].join(", ");
        let sql = format!("INSERT OR REPLACE INTO {table} ({list}) VALUES ({marks})");
        for row in rows {
            let mut insert = sqlx::query(&sql);
            for index in 0..columns.len() {
                insert = insert.bind(row.try_get::<Option<String>, _>(index).ok().flatten());
            }
            insert.execute(pool).await?;
        }
    }
    real.close().await;
    Ok(())
}

fn prepare_store(real: &Path, sandbox: &Path) -> Result<Value> {
    let mut store: Value = std::fs::read_to_string(real.join("store.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    if !store.is_object() {
        store = json!({});
    }
    // The replay records Copilot's debug stats; everything else stays as the
    // user has it.
    store[crate::copilot::DEBUG_KEY] = Value::Bool(true);
    std::fs::write(
        sandbox.join("store.json"),
        serde_json::to_vec_pretty(&store)?,
    )?;
    Ok(store)
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

fn ffmpeg() -> Result<PathBuf> {
    crate::audio::ffmpeg::find_ffmpeg_path().ok_or_else(|| anyhow!("ffmpeg was not found"))
}

/// The recording's length, from ffmpeg's header dump.
fn probe_duration(audio: &Path) -> Result<f64> {
    let output = Command::new(ffmpeg()?)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(audio)
        .output()?;
    let text = String::from_utf8_lossy(&output.stderr);
    let duration = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Duration: "))
        .and_then(|rest| rest.split(',').next())
        .ok_or_else(|| anyhow!("ffmpeg reported no duration for {}", audio.display()))?;
    let parts: Vec<f64> = duration
        .split(':')
        .map(|p| p.trim().parse().unwrap_or(0.0))
        .collect();
    match parts.as_slice() {
        [h, m, s] => Ok(h * 3600.0 + m * 60.0 + s),
        _ => bail!("unreadable duration {duration:?}"),
    }
}

/// How the fed audio reaches the live worker.
enum Feed {
    /// Nemotron: continuous windows per source.
    Streaming,
    /// Chunked engine: Silero VAD segments, as the pipeline sends them.
    Chunked(Box<crate::audio::vad::ContinuousVadProcessor>),
}

struct Feeder {
    sender: UnboundedSender<AudioChunk>,
    feed: Feed,
    chunk_id: u64,
}

impl Feeder {
    fn send(&mut self, data: Vec<f32>, device_type: RecordingDeviceType, position: u64) {
        let chunk = AudioChunk {
            data,
            sample_rate: SAMPLE_RATE,
            timestamp: position as f64 / SAMPLE_RATE as f64,
            chunk_id: self.chunk_id,
            device_type,
        };
        self.chunk_id += 1;
        let _ = self.sender.send(chunk);
    }

    fn window(&mut self, samples: &[f32], position: u64) {
        match &mut self.feed {
            Feed::Streaming => {
                self.send(
                    vec![0.0; samples.len()],
                    RecordingDeviceType::Microphone,
                    position,
                );
                self.send(samples.to_vec(), RecordingDeviceType::System, position);
            }
            Feed::Chunked(vad) => {
                let segments = vad.process_audio(samples).unwrap_or_default();
                self.segments(segments);
            }
        }
    }

    fn segments(&mut self, segments: Vec<crate::audio::vad::SpeechSegment>) {
        for segment in segments {
            if segment.samples.len() < 800 {
                continue;
            }
            let chunk = AudioChunk {
                data: segment.samples,
                sample_rate: 16_000,
                timestamp: segment.start_timestamp_ms / 1000.0,
                chunk_id: self.chunk_id,
                device_type: RecordingDeviceType::System,
            };
            self.chunk_id += 1;
            let _ = self.sender.send(chunk);
        }
    }

    /// The stop path's flush: close the utterances in progress.
    fn flush(mut self, position: u64) {
        match &mut self.feed {
            Feed::Streaming => {
                self.send(Vec::new(), RecordingDeviceType::Microphone, position);
                self.send(Vec::new(), RecordingDeviceType::System, position);
            }
            Feed::Chunked(vad) => {
                let segments = vad.flush().unwrap_or_default();
                self.segments(segments);
            }
        }
    }
}

/// Waits until the live worker has decoded up to `fed_s` minus a little
/// look-ahead, so unpaced feeding never builds a backlog the worker would
/// drop. Gives up after a while (a stuck worker must not hang the replay).
fn wait_for_decode(fed_s: f64, streaming: bool) {
    const LOOKAHEAD_S: f64 = 1.2;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        let caught_up = if streaming {
            crate::audio::live_streaming::worker::decode_totals().1 >= fed_s - LOOKAHEAD_S
        } else {
            crate::audio::live_streaming::worker::current_backlog_seconds() < 2.0
        };
        if caught_up {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Moves meeting time to `end_ms` of audio: at once while no model work is in
/// flight, at the wall clock's rate while it is (or always, in real time).
fn pace_to(end: Duration, realtime: bool) {
    use crate::clock::simulation;
    loop {
        if realtime || crate::clock::is_busy() {
            if !simulation::is_following() {
                simulation::follow_wall(true);
            }
            if simulation::elapsed() >= end {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        } else {
            simulation::advance_to(end);
            return;
        }
    }
}

/// Decodes and feeds the recording. Returns the feeder (for the stop flush)
/// and the samples fed.
fn feed_audio(
    audio: PathBuf,
    mut feeder: Feeder,
    realtime: bool,
    limit_ms: Option<u64>,
    recorder: Arc<Recorder>,
) -> Result<(Feeder, u64)> {
    let streaming = matches!(feeder.feed, Feed::Streaming);
    let mut child = Command::new(ffmpeg()?)
        .args(["-hide_banner", "-nostdin", "-v", "error", "-i"])
        .arg(&audio)
        .args(["-f", "f32le", "-ac", "1", "-ar", "48000", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting ffmpeg")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no ffmpeg stdout"))?;
    let mut bytes = vec![0u8; WINDOW_SAMPLES * 4];
    let mut position: u64 = 0;
    loop {
        if limit_ms.is_some_and(|limit| position * 1000 / SAMPLE_RATE as u64 >= limit) {
            break;
        }
        // Read one window (the last may be short).
        let mut filled = 0;
        while filled < bytes.len() {
            match stdout.read(&mut bytes[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        let count = filled / 4;
        if count == 0 {
            break;
        }
        let samples: Vec<f32> = bytes[..count * 4]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        // A window is sent once it has been "captured".
        let end = position + count as u64;
        pace_to(
            Duration::from_secs_f64(end as f64 / SAMPLE_RATE as f64),
            realtime,
        );
        feeder.window(&samples, position);
        position = end;
        if !realtime {
            wait_for_decode(position as f64 / SAMPLE_RATE as f64, streaming);
        }
        recorder
            .audio_ms
            .store(position * 1000 / SAMPLE_RATE as u64, Ordering::Relaxed);
        if count < WINDOW_SAMPLES {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok((feeder, position))
}

// ---------------------------------------------------------------------------
// Shared state the listeners fill
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Shared {
    finals: Mutex<Vec<Value>>,
    timeline: Mutex<Vec<TopicEntry>>,
    timeline_status: Mutex<String>,
    final_pass: Mutex<Option<Value>>,
    /// Start (ms) of the latest finalized Others line.
    last_others_ms: AtomicI64,
}

fn register_listeners(
    app: &AppHandle<MockRuntime>,
    recorder: &Arc<Recorder>,
    shared: &Arc<Shared>,
) {
    for &name in UI_EVENTS {
        let recorder = recorder.clone();
        let shared = shared.clone();
        app.listen_any(name, move |event| {
            let payload: Value = serde_json::from_str(event.payload())
                .unwrap_or_else(|_| Value::String(event.payload().to_string()));
            match name {
                "transcript-update" => {
                    let partial = payload
                        .get("is_partial")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !partial {
                        if payload.get("source").and_then(Value::as_str) == Some("system") {
                            let start = payload
                                .get("audio_start_time")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0);
                            shared
                                .last_others_ms
                                .store((start * 1000.0).round() as i64, Ordering::Relaxed);
                        }
                        shared
                            .finals
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(payload.clone());
                    }
                }
                "live-timeline-update" => {
                    let entries = payload
                        .get("entries")
                        .and_then(Value::as_array)
                        .map(|entries| {
                            entries
                                .iter()
                                .map(|e| TopicEntry {
                                    kind: e
                                        .get("kind")
                                        .and_then(Value::as_str)
                                        .unwrap_or("point")
                                        .to_string(),
                                    text: e
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    *shared.timeline.lock().unwrap_or_else(|e| e.into_inner()) = entries;
                    *shared
                        .timeline_status
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = payload
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
                "final-transcript-pass-complete" => {
                    *shared.final_pass.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(payload.clone());
                }
                _ => {}
            }
            recorder.write(RecordType::Event, name, payload);
        });
    }
}

// ---------------------------------------------------------------------------
// Monitors
// ---------------------------------------------------------------------------

/// Live transcription health every second, memory every five.
fn spawn_health(recorder: Arc<Recorder>, done: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use sysinfo::{ProcessesToUpdate, System};
        let mut system = System::new();
        let me = sysinfo::get_current_pid().ok();
        let mut recent: std::collections::VecDeque<(f64, f64)> = Default::default();
        let mut tick = 0u64;
        while !done.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            tick += 1;
            let (busy, audio) = crate::audio::live_streaming::worker::decode_totals();
            recent.push_back((busy, audio));
            if recent.len() > 41 {
                recent.pop_front();
            }
            let (busy0, audio0) = recent.front().copied().unwrap_or((0.0, 0.0));
            let rtf = if audio - audio0 > 1.0 {
                Some((busy - busy0) / (audio - audio0))
            } else {
                None
            };
            recorder.debug(
                debug::LIVE,
                json!({
                    "backlog_s": crate::audio::live_streaming::worker::current_backlog_seconds(),
                    "rtf": rtf,
                    "rtf_total": if audio > 0.0 { Some(busy / audio) } else { None },
                    "busy_s": busy,
                    "decoded_audio_s": audio,
                    "fed_audio_s": recorder.audio_ms.load(Ordering::Relaxed) as f64 / 1000.0,
                    "llm_busy": crate::clock::is_busy(),
                }),
            );
            if tick % 20 == 0 {
                system.refresh_processes(ProcessesToUpdate::All, true);
                let app = me
                    .and_then(|pid| system.process(pid))
                    .map(|p| p.memory())
                    .unwrap_or(0);
                let helper: u64 = system
                    .processes()
                    .values()
                    .filter(|p| p.parent() == me)
                    .filter(|p| p.name().to_string_lossy().contains("llama-helper"))
                    .map(|p| p.memory())
                    .sum();
                recorder.debug(
                    debug::MEMORY,
                    json!({
                        "app_rss_mb": app / 1_048_576,
                        "helper_rss_mb": helper / 1_048_576,
                    }),
                );
                recorder.flush();
            }
        }
    })
}

/// Runs the scripted Copilot steps and feeds the user's notes as the
/// recording reaches them.
fn spawn_script(
    recorder: Arc<Recorder>,
    shared: Arc<Shared>,
    steps: Vec<Scheduled>,
    notes: Vec<(f64, bool, String)>,
    done: Arc<AtomicBool>,
    tags: Arc<Mutex<HashMap<String, Option<String>>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut steps = steps.into_iter().peekable();
        let mut notes_fed = 0usize;
        while !done.load(Ordering::Relaxed) {
            let audio_ms = meeting_ms();
            let due = notes
                .iter()
                .take_while(|(at, _, _)| *at * 1000.0 <= audio_ms as f64)
                .count();
            if due > notes_fed {
                notes_fed = due;
                let text = notes[..due]
                    .iter()
                    .map(|(at, important, text)| {
                        format!(
                            "[{}]{}\n{}",
                            super::events::clock(*at),
                            if *important { " IMPORTANT" } else { "" },
                            text
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let _ = crate::copilot::copilot_set_notes(text.clone()).await;
                // The text too, so the viewer's live Copilot sees the notes
                // as they stood at any moment.
                recorder.debug(
                    debug::SCRIPT,
                    json!({ "kind": "notes", "count": due, "text": text }),
                );
            }
            while let Some(step) = steps.next_if(|s| s.at_ms <= audio_ms) {
                let topic =
                    script::pick_topic(&shared.timeline.lock().unwrap_or_else(|e| e.into_inner()));
                let text = script::fill_topic(&step.step.text, &topic);
                let result = match step.step.kind {
                    StepKind::Ask => crate::copilot::copilot_ask(None, Some(text.clone())).await,
                    StepKind::Action => crate::copilot::copilot_ask(Some(text.clone()), None).await,
                };
                let (id, error) = match result {
                    Ok(id) => (Some(id), None),
                    Err(error) => (None, Some(error)),
                };
                if let Some(id) = &id {
                    tags.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id.clone(), step.step.tag.clone());
                }
                println!(
                    "[replay] script {} {} at {}{}",
                    step.step.kind.label(),
                    step.step.tag.as_deref().unwrap_or(""),
                    super::events::clock(step.at_ms as f64 / 1000.0),
                    error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
                );
                recorder.debug(
                    debug::SCRIPT,
                    json!({
                        "at_ms": step.at_ms,
                        "kind": step.step.kind.label(),
                        "tag": step.step.tag,
                        "text": text,
                        "message_id": id,
                        "error": error,
                    }),
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}

/// `(seconds, important, text)` from a `live-notes.json`, in time order.
fn load_notes(path: &Path) -> Result<(Vec<(f64, bool, String)>, String)> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut notes: Vec<(f64, bool, String)> = value["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let text = n["text"].as_str()?.trim().to_string();
            (!text.is_empty()).then(|| {
                (
                    n["timestampSeconds"].as_f64().unwrap_or(0.0),
                    n["important"].as_bool().unwrap_or(false),
                    text,
                )
            })
        })
        .collect();
    notes.sort_by(|a, b| a.0.total_cmp(&b.0));
    // What the summary gets: the document's markdown, else the notes.
    let final_text = value["rawMarkdown"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            notes
                .iter()
                .map(|(at, important, text)| {
                    format!(
                        "[{}]{}\n{}",
                        super::events::clock(*at),
                        if *important { " IMPORTANT" } else { "" },
                        text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        });
    Ok((notes, final_text))
}

fn speaker_label(speaker: &str) -> String {
    match speaker {
        "mic" => "You".into(),
        "system" => "Others".into(),
        other => other.to_string(),
    }
}

/// `[mm:ss] [Speaker] text` per line, as `formatSummaryTranscript` does.
fn summary_text(lines: &[Value]) -> String {
    lines
        .iter()
        .filter_map(|l| {
            let text = l["text"].as_str()?.trim();
            if text.is_empty() {
                return None;
            }
            let mut parts = Vec::new();
            if let Some(start) = l["start"].as_f64().filter(|s| *s >= 0.0) {
                let total = start.floor() as u64;
                parts.push(format!("[{:02}:{:02}]", total / 60, total % 60));
            }
            if let Some(speaker) = l["speaker"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                parts.push(format!("[{}]", speaker_label(speaker)));
            }
            parts.push(text.to_string());
            Some(parts.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn transcript_lines(pool: &sqlx::SqlitePool, meeting_id: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT audio_start_time, audio_end_time, speaker, transcript FROM transcripts \
         WHERE meeting_id = ? ORDER BY audio_start_time",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "start": row.try_get::<Option<f64>, _>(0).ok().flatten(),
                "end": row.try_get::<Option<f64>, _>(1).ok().flatten(),
                "speaker": row.try_get::<Option<String>, _>(2).ok().flatten(),
                "text": row.try_get::<String, _>(3).unwrap_or_default(),
            })
        })
        .collect())
}

async fn wait_for<F: Fn() -> bool>(limit: Duration, ready: F) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if ready() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    ready()
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Runs one replay and returns its output folder.
pub fn run(options: Options) -> Result<PathBuf> {
    if !options.audio.is_file() {
        bail!("No recording at {}", options.audio.display());
    }
    let real = real_data_dir()?;
    let run_id = format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        options
            .label
            .clone()
            .unwrap_or_else(|| short_hash(&options.audio.to_string_lossy()))
    );
    let out_dir = real.join("replays").join(&run_id);
    let sandbox = out_dir.join("sandbox");
    std::fs::create_dir_all(&sandbox)?;
    println!("Replay {run_id}\n  output: {}", out_dir.display());

    let recorder = Arc::new(Recorder::new(&out_dir.join("events.jsonl"))?);
    let _ = LOG_RECORDER.set(recorder.clone());
    // Meeting time for every recording-time rule, and their decisions traced
    // into the event log.
    crate::clock::simulation::install();
    {
        let recorder = recorder.clone();
        super::trace::install(Arc::new(move |name, payload| recorder.debug(name, payload)));
    }
    let logger = ReplayLogger {
        file: Mutex::new(BufWriter::new(File::create(out_dir.join("app.log"))?)),
        last_warning: Mutex::new((String::new(), 0, 0)),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }

    // Sandbox: models linked, settings copied.
    let models = real.join("models");
    if models.is_dir() {
        link_tree(&models, &sandbox.join("models"))?;
    }
    let store = prepare_store(&real, &sandbox)?;
    let meeting_folder = sandbox.join("meeting");
    std::fs::create_dir_all(&meeting_folder)?;
    let extension = options
        .audio
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp4");
    std::os::unix::fs::symlink(
        &options.audio,
        meeting_folder.join(format!("audio.{extension}")),
    )?;

    // The app, on the mock runtime, with the sandbox as its data folder.
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!("{APP_IDENTIFIER}/replays/{run_id}/sandbox");
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(crate::summary::summary_engine::ModelManagerState(Arc::new(
            tokio::sync::Mutex::new(None),
        )))
        .build(context)
        .map_err(|e| anyhow!("building the headless app: {e}"))?;
    let handle = app.handle().clone();
    let resolved = handle.path().app_data_dir()?;
    if resolved != sandbox {
        bail!(
            "the sandbox resolved to {} instead of {}",
            resolved.display(),
            sandbox.display()
        );
    }

    let result = tauri::async_runtime::block_on(replay(
        handle,
        options,
        recorder.clone(),
        sandbox,
        meeting_folder,
        store,
    ));
    if let Err(error) = &result {
        recorder.write(
            RecordType::Meta,
            "run-error",
            json!({ "error": format!("{error:#}") }),
        );
    }
    super::trace::uninstall();
    recorder.flush();
    log::logger().flush();

    let records = recorder.records();
    let scored = report::score(&records);
    std::fs::write(
        out_dir.join("report.json"),
        serde_json::to_vec_pretty(&scored)?,
    )?;
    std::fs::write(
        out_dir.join("report.md"),
        report::to_markdown(&scored, false),
    )?;
    if result.is_ok() {
        println!("{}", report::to_markdown(&scored, false));
    }
    drop(app);
    result.map(|_| out_dir)
}

fn short_hash(text: &str) -> String {
    use sha2::Digest;
    hex::encode(&sha2::Sha256::digest(text.as_bytes())[..4])
}

async fn replay(
    app: AppHandle<MockRuntime>,
    options: Options,
    recorder: Arc<Recorder>,
    sandbox: PathBuf,
    meeting_folder: PathBuf,
    store: Value,
) -> Result<()> {
    // Database: a fresh sandbox one with the user's model settings.
    let db = DatabaseManager::new(
        &sandbox.join("meeting_minutes.sqlite").to_string_lossy(),
        &sandbox.join("no-legacy.db").to_string_lossy(),
    )
    .await
    .context("creating the sandbox database")?;
    copy_settings(&real_data_dir()?.join("meeting_minutes.sqlite"), db.pool()).await?;
    let pool = db.pool().clone();
    app.manage(AppState { db_manager: db });

    // What the app's setup does for transcription.
    crate::whisper_engine::commands::set_models_directory(&app);
    crate::parakeet_engine::commands::set_models_directory(&app);
    if let Some(vocabulary) = store.get("transcriptionVocabulary").and_then(Value::as_str) {
        crate::whisper_engine::whisper_engine::set_transcription_vocabulary(vocabulary.to_string());
    }
    crate::audio::transcription_queue::init_queue_worker(&app);

    let shared = Arc::new(Shared::default());
    register_listeners(&app, &recorder, &shared);

    let duration_s = probe_duration(&options.audio)?;
    let fed_s = options
        .limit_seconds
        .map(|l| l.min(duration_s))
        .unwrap_or(duration_s);
    let steps = match &options.script {
        Some(path) => script::parse(&std::fs::read_to_string(path)?)?,
        None => script::parse(script::DEFAULT_SCRIPT)?,
    };
    let (scheduled, dropped) = script::schedule(&steps, (fed_s * 1000.0) as u64);
    let notes_path = options.notes.clone().or_else(|| {
        options.use_notes.then(|| {
            options
                .audio
                .parent()
                .map(|p| p.join("live-notes.json"))
                .unwrap_or_default()
        })
    });
    let (notes, final_notes) = match notes_path.filter(|p| p.is_file()) {
        Some(path) => load_notes(&path)?,
        None => (Vec::new(), String::new()),
    };
    let model = crate::database::repositories::setting::SettingsRepository::get_model_config(&pool)
        .await
        .ok()
        .flatten()
        .map(|s| format!("{} / {}", s.provider, s.model))
        .unwrap_or_else(|| "not configured".into());

    // Start like `start_recording`: plan the live model, start its worker,
    // then the recording-time features.
    let session_id = crate::meeting_activity::new_recording_session_id();
    crate::meeting_activity::start_recording(&app, session_id.clone());
    crate::audio::transcription::live::prepare_for_recording(&app).await;
    let streaming = crate::audio::pipeline::LIVE_STREAMING_MODE.load(Ordering::SeqCst);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (worker, live_model) =
        match crate::audio::transcription::live::start_worker(&app, receiver).await {
            Ok((handle, model)) => (Some(handle), model),
            Err(_) => (None, None),
        };
    let live_engine = match (&worker, &live_model) {
        (None, _) => "none (live text off)".to_string(),
        (Some(_), Some(m)) => format!("{} / {}", m.provider, m.model),
        (Some(_), None) => "unknown".to_string(),
    };

    let mut limitations = vec![
        "The recording is one mixed track (microphone and system together). It is fed as the system (Others) source with a silent microphone, so every line is attributed to Others and the detector sees the user's own questions too.".to_string(),
    ];
    if !options.realtime {
        limitations.push("Fed as fast as live transcription decodes, on a simulated meeting clock that runs at real speed only while model work is in flight. Model latencies are real; live transcription's real-time contention is approximated (RTF is decode time over audio time), not measured.".to_string());
    }
    if !dropped.is_empty() {
        limitations.push(format!(
            "{} scripted steps were past the end of the recording and did not run.",
            dropped.len()
        ));
    }
    if worker.is_none() {
        limitations.push("No live transcription engine was available.".into());
    }
    recorder.write(
        RecordType::Meta,
        "run-start",
        json!({
            "schema_version": SCHEMA_VERSION,
            "audio_name": options.audio.file_name().map(|n| n.to_string_lossy().to_string()),
            "duration_s": duration_s,
            "fed_s": fed_s,
            "speed": if options.realtime { 1.0 } else { 0.0 },
            "realtime": options.realtime,
            "live_engine": live_engine,
            "summary_model": model,
            "script": scheduled.iter().map(|s| json!({
                "at_ms": s.at_ms, "kind": s.step.kind.label(), "tag": s.step.tag,
            })).collect::<Vec<_>>(),
            "notes": notes.len(),
            "limitations": limitations,
            "started_at": chrono::Local::now().to_rfc3339(),
        }),
    );

    crate::audio::recording_commands::set_replay_recording(true, false);
    crate::meeting_activity::set_recording_status(
        &app,
        &session_id,
        crate::meeting_activity::ActivityStatus::Recording,
    );
    crate::copilot::start_session(&app, session_id.clone());
    crate::live_summary::start_session(&app, session_id.clone());
    // The frontend creates the meeting right after start and binds it.
    let meeting_id =
        crate::database::repositories::transcript::TranscriptsRepository::save_transcript(
            &pool,
            "Replay",
            &[],
            Some(meeting_folder.to_string_lossy().to_string()),
            None,
            false,
        )
        .await?;
    crate::meeting_activity::bind_active_recording_meeting(
        app.clone(),
        session_id.clone(),
        meeting_id.clone(),
    )
    .map_err(|e| anyhow!(e))?;
    recorder.phase("live-start", json!({ "streaming": streaming }));

    let done = Arc::new(AtomicBool::new(false));
    let tags: Arc<Mutex<HashMap<String, Option<String>>>> = Default::default();
    let health = spawn_health(recorder.clone(), done.clone());
    let script_task = spawn_script(
        recorder.clone(),
        shared.clone(),
        scheduled,
        notes,
        done.clone(),
        tags,
    );

    // Feed the recording.
    let feed = if streaming {
        Feed::Streaming
    } else {
        Feed::Chunked(Box::new(
            crate::audio::vad::ContinuousVadProcessor::new(SAMPLE_RATE, 500)
                .map_err(|e| anyhow!("VAD: {e}"))?,
        ))
    };
    let feeder = Feeder {
        sender,
        feed,
        chunk_id: 0,
    };
    let (audio, realtime, limit) = (
        options.audio.clone(),
        options.realtime,
        options.limit_seconds.map(|s| (s * 1000.0) as u64),
    );
    let feed_recorder = recorder.clone();
    let (feeder, position) = tokio::task::spawn_blocking(move || {
        feed_audio(audio, feeder, realtime, limit, feed_recorder)
    })
    .await??;
    // After the audio, meeting time runs with the wall clock (stop graces,
    // the final pass and the summaries are measured as they really run).
    crate::clock::simulation::follow_wall(true);
    recorder.phase("audio-end", json!({}));

    // Stop, in `stop_recording`'s order.
    recorder.phase("stop", json!({}));
    crate::copilot::deactivate_session(&session_id);
    crate::audio::recording_commands::set_replay_recording(true, true);
    crate::meeting_activity::set_recording_status(
        &app,
        &session_id,
        crate::meeting_activity::ActivityStatus::Saving,
    );
    feeder.flush(position);
    if let Some(mut worker) = worker {
        let limit = if streaming {
            DRAIN_LIMIT
        } else {
            Duration::from_secs(600)
        };
        if tokio::time::timeout(limit, &mut worker).await.is_err() {
            log::warn!("Replay: live transcription still draining after {limit:?}; cancelling");
            crate::audio::live_streaming::worker::cancel_current();
            if tokio::time::timeout(Duration::from_secs(5), &mut worker)
                .await
                .is_err()
            {
                worker.abort();
            }
        }
    }
    let _ = crate::audio::transcription::live::release_after_recording();
    crate::live_summary::finish_session(&session_id);
    crate::copilot::finish_session(&app, &session_id);
    crate::audio::recording_commands::set_replay_recording(false, false);
    crate::meeting_activity::finish_recording(&app, &session_id, None);
    done.store(true, Ordering::Relaxed);
    let _ = script_task.await;
    recorder.phase("stopped", json!({}));

    // Save the live transcript as the frontend does.
    let finals = shared
        .finals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let segments: Vec<crate::api::TranscriptSegment> = finals
        .iter()
        .enumerate()
        .filter_map(|(index, f)| {
            let start = f["audio_start_time"].as_f64();
            let end = f["audio_end_time"].as_f64();
            Some(crate::api::TranscriptSegment {
                id: format!("live-{index}"),
                text: f["text"].as_str()?.to_string(),
                timestamp: f["timestamp"].as_str().unwrap_or_default().to_string(),
                speaker: f["source"].as_str().map(str::to_string),
                audio_start_time: start,
                audio_end_time: end,
                duration: f["duration"].as_f64(),
            })
        })
        .collect();
    crate::database::repositories::transcript::TranscriptsRepository::save_transcript(
        &pool,
        "Replay",
        &segments,
        Some(meeting_folder.to_string_lossy().to_string()),
        Some(&meeting_id),
        false,
    )
    .await?;
    // Let the live summary finish its last window (it has a 15 s grace).
    wait_for(Duration::from_secs(30), || {
        shared
            .timeline_status
            .lock()
            .map(|s| s.as_str() == "stopped" || s.is_empty())
            .unwrap_or(true)
    })
    .await;
    // Copilot's finish (persisting moments) and the last state.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The final pass (and diarization inside it).
    let mut lines = transcript_lines(&pool, &meeting_id).await?;
    if !options.skip_final {
        recorder.phase("final-pass-start", json!({}));
        let started = crate::audio::final_pass::start_final_transcript_pass(
            app.clone(),
            meeting_id.clone(),
            meeting_folder.to_string_lossy().to_string(),
            None,
            None,
        )
        .await
        .map_err(|e| anyhow!(e))?;
        let started = serde_json::to_value(&started)?;
        if started["status"] == "queued" {
            wait_for(Duration::from_secs(5400), || {
                shared
                    .final_pass
                    .lock()
                    .map(|f| f.is_some())
                    .unwrap_or(false)
            })
            .await;
        }
        let outcome = shared
            .final_pass
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        recorder.phase(
            "final-pass-done",
            json!({ "started": started, "outcome": outcome }),
        );
        lines = transcript_lines(&pool, &meeting_id).await?;
    }
    recorder.debug(debug::FINAL_TRANSCRIPT, json!({ "lines": lines }));

    // The summary, `summary_runs` times on the same input.
    let config =
        crate::database::repositories::setting::SettingsRepository::get_model_config(&pool)
            .await
            .ok()
            .flatten();
    match config {
        None => log::warn!("Replay: no summary model configured; skipping the summary"),
        Some(config) => {
            let text = summary_text(&lines);
            for run in 1..=options.summary_runs {
                recorder.phase("summary-start", json!({ "run": run }));
                let started = Instant::now();
                let outcome = summarize(
                    &app,
                    &pool,
                    &meeting_id,
                    &text,
                    &config.provider,
                    &config.model,
                    &final_notes,
                )
                .await;
                let ms = started.elapsed().as_millis() as u64;
                let payload = match outcome {
                    Ok((status, markdown, error)) => json!({
                        "run": run, "status": status, "markdown": markdown, "error": error, "ms": ms,
                    }),
                    Err(error) => json!({
                        "run": run, "status": "error", "markdown": "", "error": format!("{error:#}"), "ms": ms,
                    }),
                };
                recorder.debug(debug::SUMMARY, payload);
                recorder.phase("summary-done", json!({ "run": run }));
            }
        }
    }

    done.store(true, Ordering::Relaxed);
    let _ = health.await;
    let _ = crate::summary::summary_engine::client::shutdown_sidecar_gracefully().await;
    recorder.phase("run-end", json!({}));
    recorder.write(RecordType::Meta, "run-end", json!({}));
    Ok(())
}

/// One summary through the app's command, polled to the end like the UI.
async fn summarize(
    app: &AppHandle<MockRuntime>,
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    text: &str,
    provider: &str,
    model: &str,
    notes: &str,
) -> Result<(String, String, Option<String>)> {
    crate::summary::commands::api_process_transcript(
        app.clone(),
        app.state::<AppState>(),
        text.to_string(),
        provider.to_string(),
        model.to_string(),
        Some(meeting_id.to_string()),
        None,
        None,
        Some(notes.to_string()),
        Some("standard_meeting".to_string()),
        None,
        None,
    )
    .await
    .map_err(|e| anyhow!(e))?;
    let started = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let row =
            sqlx::query("SELECT status, result, error FROM summary_processes WHERE meeting_id = ?")
                .bind(meeting_id)
                .fetch_optional(pool)
                .await?;
        if let Some(row) = row {
            let status: String = row.try_get(0).unwrap_or_default();
            let status = status.to_lowercase();
            if !matches!(
                status.as_str(),
                "pending" | "processing" | "started" | "running"
            ) {
                let result: Option<String> = row.try_get(1).ok().flatten();
                let markdown = result
                    .as_deref()
                    .and_then(|r| serde_json::from_str::<Value>(r).ok())
                    .and_then(|v| v["markdown"].as_str().map(str::to_string))
                    .or(result)
                    .unwrap_or_default();
                let error: Option<String> = row.try_get(2).ok().flatten();
                return Ok((status, markdown, error));
            }
        }
        if started.elapsed() > Duration::from_secs(3600) {
            bail!("the summary did not finish within an hour");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_lines_are_classified() {
        assert_eq!(
            classify_sidecar("Pre-empting background llama-helper request 7"),
            Some("preempt")
        );
        assert_eq!(
            classify_sidecar("timed out; cancelling request 3 in the helper"),
            Some("cancel")
        );
        assert_eq!(classify_sidecar("Nemotron streaming model loaded"), None);
    }

    #[test]
    fn summary_text_matches_the_frontend_format() {
        let lines = vec![
            json!({"start": 65.4, "speaker": "mic", "text": " Hello "}),
            json!({"start": 70.0, "speaker": "Speaker 2", "text": "Hi"}),
            json!({"start": null, "speaker": "system", "text": "No time"}),
            json!({"start": 80.0, "speaker": null, "text": ""}),
        ];
        assert_eq!(
            summary_text(&lines),
            "[01:05] [You] Hello\n[01:10] [Speaker 2] Hi\n[Others] No time"
        );
    }
}
