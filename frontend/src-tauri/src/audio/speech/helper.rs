//! The `speech-helper` sidecar process and its wire protocol.
//!
//! `speech-helper live|batch --models DIR --compute gpu|ane|cpu|all` loads
//! FluidAudio's Core ML models from `DIR` (never from the network) and prints
//! `{"ready":true,...}` once it can take requests, or `{"error":...}` and
//! exits. Requests are binary frames on its stdin,
//! `op: u8, id: u32 LE, length: u32 LE, payload` (payload at most 64 MiB);
//! replies are JSON lines on its stdout. The live role (op 1-7) answers only
//! `decode`, so feeding audio costs a pipe write; the batch role (op 7-10)
//! answers every request with a line carrying its id. The protocol is
//! documented in `speech-helper/Sources/SpeechCore/Protocol.swift`.

use std::fmt;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use log::{info, warn};
use serde_json::Value;

pub mod op {
    // Live role; the id is the stream id.
    pub const OPEN: u8 = 1;
    pub const AUDIO: u8 = 2;
    pub const FINISH: u8 = 3;
    pub const RESET: u8 = 4;
    pub const CLOSE: u8 = 5;
    pub const DECODE: u8 = 6;
    // Both roles.
    pub const SHUTDOWN: u8 = 7;
    // Batch role; the id is the request id.
    pub const TRANSCRIBE: u8 = 8;
    pub const VAD: u8 = 9;
    pub const DIARIZE: u8 = 10;
}

/// The largest payload the helper accepts in one frame.
pub const MAX_PAYLOAD: usize = 64 << 20;

/// First load of a model compiles it for the device; later loads hit the
/// system's cache.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a live decode may take before the helper is considered stuck. A
/// decode covers at most the queued backlog (bounded by the worker).
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a blocking wait checks the cancel hook.
const CANCEL_POLL: Duration = Duration::from_millis(200);
/// How long `Drop` waits for the helper to exit after `shutdown`.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// One request frame.
pub fn frame(op: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + payload.len());
    out.push(op);
    out.extend_from_slice(&id.to_le_bytes());
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

/// What a helper process is for: live streaming during a meeting, or the
/// batch jobs after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Live,
    Batch,
}

impl Role {
    /// The helper's subcommand (and `prepare --role` value).
    pub fn arg(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Batch => "batch",
        }
    }
}

/// Where the Core ML models run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compute {
    Gpu,
    NeuralEngine,
    Cpu,
    All,
}

impl Compute {
    /// The helper's `--compute` value, also the device label.
    pub fn arg(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::NeuralEngine => "ane",
            Self::Cpu => "cpu",
            Self::All => "all",
        }
    }
}

impl fmt::Display for Compute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.arg())
    }
}

/// The helper's ready line.
#[derive(Debug, Clone, PartialEq)]
pub struct Ready {
    pub load_ms: f64,
    pub warm_ms: f64,
    pub compute: Compute,
}

/// The oldest macOS the helper runs on. The app itself runs on macOS 13, but
/// the helper targets 15: FluidAudio's offline diarizer is unreliable on 14.
pub const MIN_MACOS_MAJOR: u32 = 15;

/// Shown wherever transcription is unavailable because of the OS.
pub const OS_UNSUPPORTED_MESSAGE: &str = "Transcription needs macOS 15 or later.";

/// The major version of a `kern.osproductversion` string ("15.1" -> 15).
fn major_version(product_version: &str) -> Option<u32> {
    product_version.trim().split('.').next()?.parse().ok()
}

/// macOS's product version (`kern.osproductversion`, e.g. "15.1").
fn product_version() -> Option<String> {
    let name = c"kern.osproductversion";
    let mut buffer = [0u8; 64];
    let mut length = buffer.len();
    // SAFETY: `buffer` is valid for `length` bytes, and sysctlbyname writes at
    // most that many and updates `length`.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let text = &buffer[..length.min(buffer.len())];
    let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
    Some(String::from_utf8_lossy(text).into_owned())
}

/// Whether this Mac can run the helper (macOS 15 or later). Below that,
/// every speech feature reports itself unavailable and nothing spawns the
/// helper or downloads its models; recording still works. Logged once.
pub fn os_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let version = product_version();
        // An unreadable version is not a reason to turn transcription off.
        let supported = version
            .as_deref()
            .and_then(major_version)
            .is_none_or(|major| major >= MIN_MACOS_MAJOR);
        if !supported {
            warn!(
                "macOS {} is older than {MIN_MACOS_MAJOR}: transcription, speaker labels and the speech models are unavailable",
                version.as_deref().unwrap_or("?")
            );
        }
        supported
    })
}

/// The helper binary: `MINUTES_SPEECH_HELPER`, next to the app executable
/// (Tauri's externalBin), or the dev build in the repo. `None` below macOS
/// 15 (see [`os_supported`]), so nothing tries to run it there.
pub fn resolve_helper() -> Option<PathBuf> {
    if !os_supported() {
        return None;
    }
    if let Some(path) = std::env::var_os("MINUTES_SPEECH_HELPER").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("speech-helper"));
        candidates.push(dir.join("speech-helper-aarch64-apple-darwin"));
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("binaries/speech-helper-aarch64-apple-darwin"));
    candidates.push(manifest.join("../../speech-helper/.build/release/speech-helper"));
    candidates.into_iter().find(|path| path.is_file())
}

fn helper_binary() -> Result<PathBuf> {
    if !os_supported() {
        bail!(OS_UNSUPPORTED_MESSAGE);
    }
    resolve_helper().ok_or_else(|| anyhow!("the speech-helper binary was not found"))
}

/// Parse the helper's first line: ready, or the load error.
fn parse_ready(line: &str, compute: Compute) -> Result<Ready> {
    let value: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("unreadable speech-helper line: {}", line.trim()))?;
    if let Some(error) = value.get("error") {
        bail!("speech-helper: {}", error_text(error));
    }
    if value.get("ready").and_then(Value::as_bool) != Some(true) {
        bail!(
            "speech-helper sent an unexpected first line: {}",
            line.trim()
        );
    }
    Ok(Ready {
        load_ms: value.get("load_ms").and_then(Value::as_f64).unwrap_or(0.0),
        warm_ms: value.get("warm_ms").and_then(Value::as_f64).unwrap_or(0.0),
        compute,
    })
}

fn error_text(error: &Value) -> String {
    match error.as_str() {
        Some(text) => text.to_string(),
        None => error.to_string(),
    }
}

/// A reply line as seen by a request waiting on it.
#[derive(Debug, PartialEq)]
enum Line {
    /// The answer to the request.
    Reply(Value),
    /// `{"id":N,"progress":p}` for the request.
    Progress(f32),
    /// `{"error":...}`: the request (or, in the live role, an earlier
    /// reply-less one) failed.
    Failed(String),
    /// A line for another request (one that already timed out).
    Stale,
    /// Not JSON.
    Unreadable,
}

/// Classify a line for request `id`. Batch lines carry the request id; live
/// lines do not (a decode answers for every stream).
fn classify(line: &str, role: Role, id: u32) -> Line {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Line::Unreadable;
    };
    if role == Role::Batch {
        if let Some(other) = value.get("id").and_then(Value::as_u64) {
            if other != u64::from(id) {
                return Line::Stale;
            }
        }
    }
    if let Some(error) = value.get("error") {
        return Line::Failed(error_text(error));
    }
    if let Some(progress) = value.get("progress").and_then(Value::as_f64) {
        return Line::Progress(progress.clamp(0.0, 1.0) as f32);
    }
    Line::Reply(value)
}

/// Why a blocking wait on the helper ended without a line.
#[derive(Debug, PartialEq)]
enum WaitEnd {
    Timeout,
    Disconnected,
    Cancelled,
}

const CANCELLED: &str = "speech-helper request was cancelled";

struct Io {
    stdin: BufWriter<ChildStdin>,
    lines: Receiver<String>,
    /// Requests written before the helper reported ready. Writing them
    /// straight away would block on a full pipe while it loads.
    pending: Option<Vec<u8>>,
    ready: Option<Ready>,
    /// First failure of the process (not of one request); every later call
    /// reports it.
    broken: Option<String>,
}

/// A running helper process.
pub struct Helper {
    role: Role,
    compute: Compute,
    child: Mutex<Child>,
    io: Mutex<Io>,
    /// Mirrors `Io::broken`, readable while a request holds the lock.
    failed: AtomicBool,
    started: Instant,
    /// Polled during blocking waits; `true` abandons the wait and breaks the
    /// helper (see [`Helper::cancel_when`]).
    cancel: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl Helper {
    /// Start the helper on `models_dir`. Returns at once: the models load
    /// (seconds; a GPU compiles its shaders on every launch) in the child.
    /// Frames sent meanwhile are held here; [`Helper::wait_ready`] and the
    /// first exchange wait for the load.
    pub fn spawn(role: Role, models_dir: &Path, compute: Compute) -> Result<Helper> {
        let binary = helper_binary()?;
        let mut child = Command::new(&binary)
            .arg(role.arg())
            .arg("--models")
            .arg(models_dir)
            .arg("--compute")
            .arg(compute.arg())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start {}", binary.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no helper stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no helper stdout"))?;
        let label = role.arg();
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    info!("speech-helper ({label}): {line}");
                }
            });
        }
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // Replies are JSON objects; anything else is a stray log line.
                if !line.trim_start().starts_with('{') {
                    warn!("speech-helper ({label}, stdout): {line}");
                    continue;
                }
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Helper {
            role,
            compute,
            child: Mutex::new(child),
            io: Mutex::new(Io {
                stdin: BufWriter::with_capacity(1 << 16, stdin),
                lines,
                pending: Some(Vec::new()),
                ready: None,
                broken: None,
            }),
            failed: AtomicBool::new(false),
            started: Instant::now(),
            cancel: OnceLock::new(),
        })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn compute(&self) -> Compute {
        self.compute
    }

    /// Whether the process failed (load error, exit, timeout, cancel). A
    /// broken helper answers every call with that failure; start a new one.
    pub fn is_broken(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    /// Abandon blocking waits once `check` returns true (checked every
    /// 200 ms), so a Stop while a model compiles does not wait for it. The
    /// helper is broken afterwards. Only the first hook is kept.
    pub fn cancel_when(&self, check: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = self.cancel.set(Box::new(check));
    }

    /// Wait up to `timeout` for the models to load. A timeout breaks the
    /// helper.
    pub fn wait_ready(&self, timeout: Duration) -> Result<Ready> {
        let mut io = self.lock_io();
        self.settle(&mut io, Some(timeout));
        if let Some(broken) = &io.broken {
            return Err(anyhow!("{broken}"));
        }
        io.ready
            .clone()
            .ok_or_else(|| anyhow!("speech-helper is not ready"))
    }

    /// Send a request that has no reply. Writes are buffered; the next
    /// exchange flushes them.
    pub fn send(&self, op: u8, id: u32, payload: &[u8]) -> Result<()> {
        if payload.len() > MAX_PAYLOAD {
            bail!(
                "speech-helper payload of {} bytes is too large",
                payload.len()
            );
        }
        let mut io = self.lock_io();
        self.settle(&mut io, None);
        if let Some(broken) = &io.broken {
            return Err(anyhow!("{broken}"));
        }
        let bytes = frame(op, id, payload);
        if let Some(pending) = io.pending.as_mut() {
            pending.extend_from_slice(&bytes);
        } else if let Err(error) = io.stdin.write_all(&bytes) {
            let message = format!("speech-helper stopped: {error}");
            self.fail(&mut io, message.clone());
            bail!(message);
        }
        Ok(())
    }

    /// Send a request and wait up to `timeout` for its reply. An `error` line
    /// is an `Err` (the helper stays usable); a timeout or exit breaks it.
    /// Waits for the models to load first (up to [`LOAD_TIMEOUT`]).
    pub fn exchange(&self, op: u8, id: u32, payload: &[u8], timeout: Duration) -> Result<Value> {
        self.exchange_with_progress(op, id, payload, timeout, &mut |_| {})
    }

    /// [`Helper::exchange`] for requests that report progress (`diarize`):
    /// each `{"id":N,"progress":p}` line calls `on_progress(p)` (0..1) and
    /// restarts the timeout, so `timeout` bounds the silence between lines.
    pub fn exchange_with_progress(
        &self,
        op: u8,
        id: u32,
        payload: &[u8],
        timeout: Duration,
        on_progress: &mut dyn FnMut(f32),
    ) -> Result<Value> {
        if payload.len() > MAX_PAYLOAD {
            bail!(
                "speech-helper payload of {} bytes is too large",
                payload.len()
            );
        }
        let mut io = self.lock_io();
        self.settle(&mut io, Some(LOAD_TIMEOUT));
        if let Some(broken) = &io.broken {
            return Err(anyhow!("{broken}"));
        }
        let sent = io
            .stdin
            .write_all(&frame(op, id, payload))
            .and_then(|_| io.stdin.flush());
        if let Err(error) = sent {
            let message = format!("speech-helper stopped: {error}");
            self.fail(&mut io, message.clone());
            bail!(message);
        }
        let mut deadline = Instant::now() + timeout;
        loop {
            let line = match self.recv(&io.lines, deadline) {
                Ok(line) => line,
                Err(end) => {
                    let message = match end {
                        WaitEnd::Timeout => {
                            format!("speech-helper did not answer within {}s", timeout.as_secs())
                        }
                        WaitEnd::Disconnected => "speech-helper exited".to_string(),
                        WaitEnd::Cancelled => CANCELLED.to_string(),
                    };
                    self.fail(&mut io, message.clone());
                    bail!(message);
                }
            };
            match classify(&line, self.role, id) {
                Line::Reply(value) => return Ok(value),
                Line::Failed(error) => bail!("speech-helper: {error}"),
                Line::Progress(progress) => {
                    on_progress(progress);
                    deadline = Instant::now() + timeout;
                }
                Line::Stale => {
                    warn!("speech-helper: ignoring a reply to an earlier request: {line}")
                }
                Line::Unreadable => warn!("speech-helper: unreadable line: {line}"),
            }
        }
    }

    fn fail(&self, io: &mut Io, message: String) {
        io.broken = Some(message);
        self.failed.store(true, Ordering::SeqCst);
    }

    fn lock_io(&self) -> std::sync::MutexGuard<'_, Io> {
        self.io.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cancelled(&self) -> bool {
        self.cancel.get().is_some_and(|check| check())
    }

    /// Wait until `deadline` for the helper's next line, in short steps,
    /// giving up as soon as the cancel hook fires.
    fn recv(
        &self,
        lines: &Receiver<String>,
        deadline: Instant,
    ) -> std::result::Result<String, WaitEnd> {
        loop {
            if self.cancelled() {
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

    /// Handle the helper's first line (ready or an error), then send what was
    /// held back. With `wait`, block up to that long for it; otherwise only
    /// take a line that already arrived.
    fn settle(&self, io: &mut Io, wait: Option<Duration>) {
        if io.pending.is_none() || io.broken.is_some() {
            return;
        }
        let line = match wait {
            Some(timeout) => match self.recv(&io.lines, Instant::now() + timeout) {
                Ok(line) => Ok(line),
                Err(WaitEnd::Cancelled) => {
                    // Leave the helper loading; dropping it kills it.
                    self.fail(io, CANCELLED.to_string());
                    return;
                }
                Err(WaitEnd::Timeout) => Err(anyhow!(
                    "speech-helper did not load its models within {}s",
                    timeout.as_secs()
                )),
                Err(WaitEnd::Disconnected) => {
                    Err(anyhow!("speech-helper exited while loading its models"))
                }
            },
            None => match io.lines.try_recv() {
                Ok(line) => Ok(line),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    Err(anyhow!("speech-helper exited while loading its models"))
                }
            },
        };
        match line.and_then(|line| parse_ready(&line, self.compute)) {
            Ok(ready) => {
                info!(
                    "speech-helper ({}, {}) ready in {:.1}s (load {:.0} ms, warm-up {:.0} ms)",
                    self.role.arg(),
                    self.compute.arg(),
                    self.started.elapsed().as_secs_f64(),
                    ready.load_ms,
                    ready.warm_ms
                );
                io.ready = Some(ready);
                let held = io.pending.take().unwrap_or_default();
                if let Err(error) = io.stdin.write_all(&held) {
                    self.fail(io, format!("speech-helper stopped: {error}"));
                }
            }
            Err(error) => {
                warn!(
                    "speech-helper ({}, {}) failed to load: {error:#}",
                    self.role.arg(),
                    self.compute.arg()
                );
                io.pending = None;
                self.fail(io, format!("{error:#}"));
            }
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Still loading, or failed (exited, stuck, cancelled): nothing to
        // finish, so do not wait for it. A stuck helper may not read stdin,
        // so a shutdown frame could block.
        let quit = {
            let mut io = self.lock_io();
            self.settle(&mut io, None);
            if io.pending.is_some() || io.broken.is_some() {
                false
            } else {
                io.stdin
                    .write_all(&frame(op::SHUTDOWN, 0, &[]))
                    .and_then(|_| io.stdin.flush())
                    .is_ok()
            }
        };
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if quit {
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => break,
                }
            }
            warn!(
                "speech-helper ({}) did not exit; killing it",
                self.role.arg()
            );
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Load (and so compile and cache) the models of `role` for `compute` in a
/// helper that exits right after, so the first use does not pay for the
/// compile. Returns the seconds it took.
pub fn prepare(role: Role, models_dir: &Path, compute: Compute) -> Result<f64> {
    let binary = helper_binary()?;
    let started = Instant::now();
    let output = Command::new(&binary)
        .arg("prepare")
        .arg("--role")
        .arg(role.arg())
        .arg("--models")
        .arg(models_dir)
        .arg("--compute")
        .arg(compute.arg())
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not start {}", binary.display()))?;
    let prepared = parse_prepared(&String::from_utf8_lossy(&output.stdout));
    if let Err(error) = &prepared {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().rev().find(|line| !line.trim().is_empty());
        warn!(
            "speech-helper prepare ({}, {}) failed ({}): {error:#}; last log line: {}",
            role.arg(),
            compute.arg(),
            output.status,
            tail.unwrap_or("none")
        );
    }
    prepared?;
    Ok(started.elapsed().as_secs_f64())
}

/// The `prepare` subcommand's stdout: `{"prepared":true,...}` or an error.
fn parse_prepared(stdout: &str) -> Result<()> {
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| anyhow!("speech-helper prepare printed nothing"))?;
    let value: Value = serde_json::from_str(line.trim())
        .with_context(|| format!("unreadable speech-helper line: {}", line.trim()))?;
    if let Some(error) = value.get("error") {
        bail!("speech-helper: {}", error_text(error));
    }
    if value.get("prepared").and_then(Value::as_bool) != Some(true) {
        bail!(
            "speech-helper prepare sent an unexpected line: {}",
            line.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_little_endian_with_a_length() {
        let bytes = frame(op::AUDIO, 0x0102_0304, &[9, 8, 7]);
        assert_eq!(bytes, vec![2, 4, 3, 2, 1, 3, 0, 0, 0, 9, 8, 7]);
        assert_eq!(frame(op::DECODE, 0, &[]).len(), 9);
        assert_eq!(frame(op::DIARIZE, 1, &[])[0], 10);
    }

    #[test]
    fn audio_is_sent_as_f32_le() {
        let payload = audio_payload(&[1.0, -0.5]);
        assert_eq!(payload.len(), 8);
        assert_eq!(f32::from_le_bytes(payload[4..8].try_into().unwrap()), -0.5);
    }

    #[test]
    fn the_macos_major_version() {
        assert_eq!(major_version("15.1"), Some(15));
        assert_eq!(major_version("14.7.2\n"), Some(14));
        assert_eq!(major_version("26.0"), Some(26));
        assert_eq!(major_version(""), None);
        // This machine reports something parseable.
        assert!(product_version()
            .as_deref()
            .and_then(major_version)
            .is_some());
    }

    #[test]
    fn compute_and_role_arguments() {
        assert_eq!(Compute::NeuralEngine.arg(), "ane");
        assert_eq!(Compute::All.to_string(), "all");
        assert_eq!(Role::Batch.arg(), "batch");
    }

    #[test]
    fn the_ready_line_or_its_error() {
        let ready = parse_ready(
            r#"{"ready":true,"role":"live","compute":"ane","load_ms":900,"warm_ms":40}"#,
            Compute::NeuralEngine,
        )
        .unwrap();
        assert_eq!(
            ready,
            Ready {
                load_ms: 900.0,
                warm_ms: 40.0,
                compute: Compute::NeuralEngine
            }
        );
        let error = parse_ready(r#"{"error":"missing model files"}"#, Compute::Gpu).unwrap_err();
        assert!(error.to_string().contains("missing model files"));
        assert!(parse_ready(r#"{"streams":[]}"#, Compute::Gpu).is_err());
        assert!(parse_ready("not json", Compute::Gpu).is_err());
    }

    #[test]
    fn decode_replies_carry_each_streams_hypothesis() {
        let line = r#"{"streams":[{"id":2,"start":1.5,"tokens":[" Hello",","],"times":[0.4,0.48]}],"ms":3.1}"#;
        let Line::Reply(value) = classify(line, Role::Live, 0) else {
            panic!("not a reply");
        };
        assert_eq!(value["streams"][0]["id"], 2);
        assert_eq!(value["streams"][0]["tokens"][0], " Hello");
        assert_eq!(value["streams"][0]["times"][1], 0.48);
    }

    #[test]
    fn errors_progress_and_stale_lines() {
        assert_eq!(
            classify(r#"{"error":"missing stream 3"}"#, Role::Live, 0),
            Line::Failed("missing stream 3".into())
        );
        assert_eq!(
            classify(r#"{"id":4,"error":"bad wav"}"#, Role::Batch, 4),
            Line::Failed("bad wav".into())
        );
        assert_eq!(
            classify(r#"{"id":4,"progress":0.42}"#, Role::Batch, 4),
            Line::Progress(0.42_f64 as f32)
        );
        assert_eq!(
            classify(r#"{"id":4,"progress":7}"#, Role::Batch, 4),
            Line::Progress(1.0)
        );
        assert_eq!(
            classify(r#"{"id":3,"text":"late"}"#, Role::Batch, 4),
            Line::Stale
        );
        assert_eq!(
            classify(r#"{"id":3,"error":"late"}"#, Role::Batch, 4),
            Line::Stale
        );
        assert_eq!(classify("{oops", Role::Batch, 4), Line::Unreadable);
        let Line::Reply(value) = classify(
            r#"{"id":4,"segments":[{"start":0.5,"end":4.1}],"ms":12}"#,
            Role::Batch,
            4,
        ) else {
            panic!("not a reply");
        };
        assert_eq!(value["segments"][0]["end"], 4.1);
    }

    #[test]
    fn prepare_output() {
        assert!(parse_prepared(
            "{\"prepared\":true,\"role\":\"live\",\"compute\":\"ane\",\"load_ms\":1,\"warm_ms\":2}\n"
        )
        .is_ok());
        assert!(parse_prepared("{\"error\":\"no models\"}\n")
            .unwrap_err()
            .to_string()
            .contains("no models"));
        assert!(parse_prepared("").is_err());
    }
}
