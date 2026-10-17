//! Nemotron on Core ML (GPU or Neural Engine) through the `nemotron-helper`
//! sidecar (Swift, `nemotron-helper/` in the repo).
//!
//! Requests are binary frames on the helper's stdin,
//! `op: u8, stream: u32 LE, length: u32 LE, payload`; replies are JSON lines
//! on its stdout. Only `decode` has a reply, so feeding audio costs a pipe
//! write. The protocol is documented in
//! `nemotron-helper/Sources/NemotronCore/Protocol.swift`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use serde::Deserialize;

use super::recognizer::{Hypothesis, Recognizer, StreamId};

/// Where the Core ML models run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compute {
    Gpu,
    NeuralEngine,
}

impl Compute {
    /// The helper's `--compute` value, also the device label.
    pub fn arg(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::NeuralEngine => "ane",
        }
    }
}

pub mod op {
    pub const OPEN: u8 = 1;
    pub const AUDIO: u8 = 2;
    pub const FINISH: u8 = 3;
    pub const RESET: u8 = 4;
    pub const CLOSE: u8 = 5;
    pub const DECODE: u8 = 6;
    pub const SHUTDOWN: u8 = 7;
}

/// One request frame.
pub fn frame(op: u8, stream: StreamId, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + payload.len());
    out.push(op);
    out.extend_from_slice(&stream.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn audio_payload(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[derive(Debug, Deserialize)]
pub struct StreamReply {
    pub id: StreamId,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub tokens: Vec<String>,
    #[serde(default)]
    pub times: Vec<f64>,
}

#[derive(Debug, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub ready: Option<bool>,
    #[serde(default)]
    pub load_ms: Option<f64>,
    #[serde(default)]
    pub warm_ms: Option<f64>,
    #[serde(default)]
    pub streams: Option<Vec<StreamReply>>,
}

pub fn parse_reply(line: &str) -> Result<Reply> {
    let reply: Reply = serde_json::from_str(line.trim())
        .with_context(|| format!("unreadable nemotron-helper reply: {}", line.trim()))?;
    if let Some(error) = &reply.error {
        return Err(anyhow!("nemotron-helper: {error}"));
    }
    Ok(reply)
}

/// The helper binary: `MINUTES_NEMOTRON_HELPER`, next to the app executable
/// (Tauri's externalBin), or the dev build in the repo.
pub fn resolve_helper() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MINUTES_NEMOTRON_HELPER").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("nemotron-helper"));
        candidates.push(dir.join("nemotron-helper-aarch64-apple-darwin"));
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("binaries/nemotron-helper-aarch64-apple-darwin"));
    candidates.push(manifest.join("../../nemotron-helper/.build/release/nemotron-helper"));
    candidates.into_iter().find(|path| path.is_file())
}

/// How long a decode may take before the helper is considered stuck. A
/// decode covers at most the queued backlog (bounded by the worker).
const DECODE_TIMEOUT: Duration = Duration::from_secs(30);
/// First load of a model compiles it for the device; later loads hit the
/// system's cache.
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// How often a blocking wait checks whether the worker was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// Why a blocking wait on the helper ended without a line.
#[derive(Debug, PartialEq)]
enum WaitEnd {
    Timeout,
    Disconnected,
    Cancelled,
}

/// Wait up to `timeout` for the helper's next line, in short steps, giving up
/// as soon as the streaming worker is cancelled (Stop while a model compiles
/// must not wait for it).
fn recv_cancellable(
    lines: &Receiver<String>,
    timeout: Duration,
    generation: u64,
) -> std::result::Result<String, WaitEnd> {
    let deadline = Instant::now() + timeout;
    loop {
        if super::worker::CANCEL_GENERATION.load(Ordering::SeqCst) != generation {
            return Err(WaitEnd::Cancelled);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(WaitEnd::Timeout);
        }
        match lines.recv_timeout(left.min(CANCEL_POLL)) {
            Ok(line) => return Ok(line),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err(WaitEnd::Disconnected),
        }
    }
}

struct Io {
    stdin: std::io::BufWriter<ChildStdin>,
    lines: Receiver<String>,
    /// Requests written before the helper reported ready. Writing them
    /// straight away would block on a full pipe while it loads.
    pending: Option<Vec<u8>>,
    started: Instant,
    /// First failure; every later call reports it.
    broken: Option<String>,
}

pub struct NativeRecognizer {
    compute: Compute,
    child: Mutex<Child>,
    io: Mutex<Io>,
    results: Mutex<HashMap<StreamId, Hypothesis>>,
    next_id: AtomicU32,
    /// The cancel generation when this engine started; a newer one means the
    /// worker using it was cancelled.
    generation: u64,
}

impl NativeRecognizer {
    /// Start the helper on `model_dir`. Returns at once: the models load
    /// (seconds; a GPU compiles its shaders on every launch) while the
    /// recording starts. Audio sent meanwhile is held here, and the first
    /// decode waits for the load ([`Recognizer::wait_ready`]).
    pub fn spawn(helper: &Path, model_dir: &Path, compute: Compute) -> Result<Self> {
        let mut child = Command::new(helper)
            .arg("--model-dir")
            .arg(model_dir)
            .arg("--compute")
            .arg(compute.arg())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start {}", helper.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no helper stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no helper stdout"))?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    info!("nemotron-helper: {line}");
                }
            });
        }
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // Replies are JSON objects; anything else is a stray log line.
                if !line.trim_start().starts_with('{') {
                    warn!("nemotron-helper (stdout): {line}");
                    continue;
                }
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            compute,
            child: Mutex::new(child),
            io: Mutex::new(Io {
                stdin: std::io::BufWriter::with_capacity(1 << 16, stdin),
                lines,
                pending: Some(Vec::new()),
                started: Instant::now(),
                broken: None,
            }),
            results: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            generation: super::worker::CANCEL_GENERATION.load(Ordering::SeqCst),
        })
    }

    /// Handle the helper's first line (`{"ready":true}` or an error), then
    /// send what was held back. `wait` blocks for it; otherwise only a line
    /// that already arrived is taken.
    fn settle(&self, io: &mut Io, wait: bool) {
        if io.pending.is_none() || io.broken.is_some() {
            return;
        }
        let line = if wait {
            match recv_cancellable(&io.lines, LOAD_TIMEOUT, self.generation) {
                Ok(line) => Ok(line),
                Err(WaitEnd::Cancelled) => {
                    // Leave the helper loading; dropping the engine kills it.
                    io.broken = Some("live transcription was cancelled".to_string());
                    return;
                }
                Err(_) => Err(()),
            }
        } else {
            match io.lines.try_recv() {
                Ok(line) => Ok(line),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(_) => Err(()),
            }
        };
        let ready = match line {
            Ok(line) => parse_reply(&line).and_then(|reply| {
                if reply.ready == Some(true) {
                    Ok(reply)
                } else {
                    Err(anyhow!("nemotron-helper sent an unexpected first line"))
                }
            }),
            Err(()) => Err(anyhow!("nemotron-helper did not load its models")),
        };
        match ready {
            Ok(reply) => {
                info!(
                    "Nemotron Core ML ({}) ready in {:.1}s (load {:.0} ms, warm-up {:.0} ms)",
                    self.compute.arg(),
                    io.started.elapsed().as_secs_f64(),
                    reply.load_ms.unwrap_or(0.0),
                    reply.warm_ms.unwrap_or(0.0)
                );
                let held = io.pending.take().unwrap_or_default();
                if let Err(error) = io.stdin.write_all(&held) {
                    io.broken = Some(format!("nemotron-helper stopped: {error}"));
                }
            }
            Err(error) => {
                warn!("Nemotron Core ML ({}) failed to load: {error:#}", self.compute.arg());
                io.pending = None;
                io.broken = Some(format!("{error:#}"));
            }
        }
    }

    fn send(&self, op: u8, stream: StreamId, payload: &[u8]) {
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        self.settle(&mut io, false);
        if io.broken.is_some() {
            return;
        }
        let bytes = frame(op, stream, payload);
        if let Some(pending) = io.pending.as_mut() {
            pending.extend_from_slice(&bytes);
        } else if let Err(error) = io.stdin.write_all(&bytes) {
            io.broken = Some(format!("nemotron-helper stopped: {error}"));
        }
    }

    fn exchange(&self, op: u8) -> Result<Reply> {
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        self.settle(&mut io, true);
        if let Some(broken) = &io.broken {
            return Err(anyhow!("{broken}"));
        }
        let sent = io
            .stdin
            .write_all(&frame(op, 0, &[]))
            .and_then(|_| io.stdin.flush());
        if let Err(error) = sent {
            let message = format!("nemotron-helper stopped: {error}");
            io.broken = Some(message.clone());
            return Err(anyhow!(message));
        }
        match recv_cancellable(&io.lines, DECODE_TIMEOUT, self.generation) {
            Ok(line) => parse_reply(&line),
            Err(WaitEnd::Timeout) => {
                io.broken = Some("nemotron-helper stopped answering".to_string());
                Err(anyhow!("nemotron-helper stopped answering"))
            }
            Err(WaitEnd::Disconnected) => {
                io.broken = Some("nemotron-helper exited".to_string());
                Err(anyhow!("nemotron-helper exited"))
            }
            Err(WaitEnd::Cancelled) => {
                io.broken = Some("live transcription was cancelled".to_string());
                Err(anyhow!("live transcription was cancelled"))
            }
        }
    }
}

impl Recognizer for NativeRecognizer {
    fn device(&self) -> &'static str {
        self.compute.arg()
    }

    fn wait_ready(&self) -> Result<()> {
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        self.settle(&mut io, true);
        match &io.broken {
            Some(broken) => Err(anyhow!("{broken}")),
            None => Ok(()),
        }
    }

    fn create_stream(&self, sample_rate: u32) -> Result<StreamId> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Hypothesis::default());
        self.send(op::OPEN, id, &sample_rate.to_le_bytes());
        Ok(id)
    }

    fn accept(&self, id: StreamId, _sample_rate: u32, samples: &[f32]) {
        self.send(op::AUDIO, id, &audio_payload(samples));
    }

    fn input_finished(&self, id: StreamId) {
        self.send(op::FINISH, id, &[]);
    }

    fn decode(&self, _ids: &[StreamId]) -> Result<()> {
        // The helper decodes every open stream; ones with nothing ready are
        // a no-op there.
        let reply = self.exchange(op::DECODE)?;
        let mut results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        for stream in reply.streams.unwrap_or_default() {
            if let Some(hypothesis) = results.get_mut(&stream.id) {
                *hypothesis = Hypothesis {
                    tokens: stream.tokens,
                    times: stream.times,
                    start: stream.start,
                };
            }
        }
        Ok(())
    }

    fn result(&self, id: StreamId) -> Option<Hypothesis> {
        let results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        results.get(&id).cloned()
    }

    fn reset(&self, id: StreamId) {
        self.send(op::RESET, id, &[]);
        if let Some(hypothesis) = self
            .results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&id)
        {
            hypothesis.tokens.clear();
            hypothesis.times.clear();
        }
    }

    fn close(&self, id: StreamId) {
        self.send(op::CLOSE, id, &[]);
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}

impl Drop for NativeRecognizer {
    fn drop(&mut self) {
        let loading = {
            let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
            self.settle(&mut io, false);
            io.pending.is_some()
        };
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if loading {
            // Still loading: nothing to finish, so do not wait for it.
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        self.send(op::SHUTDOWN, 0, &[]);
        if let Ok(mut io) = self.io.lock() {
            let _ = io.stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => break,
            }
        }
        warn!("nemotron-helper did not exit; killing it");
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Load (and so compile and cache) the models for `compute` in a helper that
/// exits right after, so the first recording does not pay for the compile.
pub fn prepare(helper: &Path, model_dir: &Path, compute: Compute) -> Result<f64> {
    let started = Instant::now();
    let output = Command::new(helper)
        .arg("--model-dir")
        .arg(model_dir)
        .arg("--compute")
        .arg(compute.arg())
        .arg("--prepare")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("could not start {}", helper.display()))?;
    let line = String::from_utf8_lossy(&output.stdout);
    parse_reply(line.lines().next().unwrap_or(""))?;
    Ok(started.elapsed().as_secs_f64())
}

#[cfg(test)]
mod cancel_tests {
    use super::*;

    #[test]
    fn a_cancel_ends_a_long_wait_promptly() {
        let (_sender, lines) = std::sync::mpsc::channel::<String>();
        let generation = super::super::worker::CANCEL_GENERATION.load(Ordering::SeqCst);
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let end = recv_cancellable(&lines, Duration::from_secs(60), generation);
            (end, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(100));
        super::super::worker::CANCEL_GENERATION.fetch_add(1, Ordering::SeqCst);
        let (end, took) = waiter.join().unwrap();
        assert_eq!(end, Err(WaitEnd::Cancelled));
        assert!(took < Duration::from_secs(2), "took {took:?}");
    }

    #[test]
    fn a_line_still_arrives_through_the_cancellable_wait() {
        let (sender, lines) = std::sync::mpsc::channel::<String>();
        sender.send("{\"ready\":true}".to_string()).unwrap();
        let generation = super::super::worker::CANCEL_GENERATION.load(Ordering::SeqCst);
        assert_eq!(
            recv_cancellable(&lines, Duration::from_secs(1), generation),
            Ok("{\"ready\":true}".to_string())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_little_endian_with_a_length() {
        let bytes = frame(op::AUDIO, 0x0102_0304, &[9, 8, 7]);
        assert_eq!(bytes, vec![2, 4, 3, 2, 1, 3, 0, 0, 0, 9, 8, 7]);
        assert_eq!(frame(op::DECODE, 0, &[]).len(), 9);
    }

    #[test]
    fn audio_is_sent_as_f32_le() {
        let payload = audio_payload(&[1.0, -0.5]);
        assert_eq!(payload.len(), 8);
        assert_eq!(f32::from_le_bytes(payload[4..8].try_into().unwrap()), -0.5);
    }

    #[test]
    fn decode_replies_carry_each_streams_hypothesis() {
        let reply = parse_reply(
            r#"{"streams":[{"id":2,"start":1.5,"tokens":[" Hello",","],"times":[0.4,0.48]}],"ms":3.1}"#,
        )
        .unwrap();
        let streams = reply.streams.unwrap();
        assert_eq!(streams[0].id, 2);
        assert_eq!(streams[0].tokens, vec![" Hello", ","]);
        assert_eq!(streams[0].times, vec![0.4, 0.48]);
        assert_eq!(streams[0].start, 1.5);
    }

    #[test]
    fn helper_errors_become_errors() {
        let error = parse_reply(r#"{"error":"missing stream 3"}"#).unwrap_err();
        assert!(error.to_string().contains("missing stream 3"));
        assert!(parse_reply("not json").is_err());
        let ready = parse_reply(r#"{"ready":true,"compute":"gpu","load_ms":900,"warm_ms":40}"#)
            .unwrap();
        assert_eq!(ready.ready, Some(true));
    }

    /// Against the real helper and Core ML bundle: set
    /// `NEMOTRON_COREML_MODEL_DIR` (and build `nemotron-helper`), then run
    /// with `--ignored`.
    #[test]
    #[ignore]
    fn the_helper_decodes_streams_over_the_pipe() {
        let (Ok(dir), Some(helper)) = (std::env::var("NEMOTRON_COREML_MODEL_DIR"), resolve_helper())
        else {
            return;
        };
        let recognizer =
            NativeRecognizer::spawn(&helper, Path::new(&dir), Compute::NeuralEngine).unwrap();
        let a = recognizer.create_stream(48_000).unwrap();
        let b = recognizer.create_stream(48_000).unwrap();
        // A 440 Hz tone on one stream, silence on the other: decodes cleanly
        // (no words expected) and both streams answer.
        let tone: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.1)
            .collect();
        recognizer.accept(a, 48_000, &tone);
        recognizer.accept(b, 48_000, &vec![0.0; 48_000]);
        recognizer.decode(&[a, b]).unwrap();
        recognizer.input_finished(a);
        recognizer.decode(&[a, b]).unwrap();
        assert!(recognizer.result(a).is_some());
        assert!(recognizer.result(b).is_some());
        recognizer.close(a);
        recognizer.close(b);
    }
}
