// Sidecar process lifecycle management for llama-helper
// Handles spawning, health checking, keep-alive, and graceful shutdown

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use super::models;

// ============================================================================
// Sidecar State Management
// ============================================================================

/// Sidecar process manager with keep-alive and health monitoring
pub struct SidecarManager {
    /// Child process handle
    child_process: Arc<Mutex<Option<Child>>>,

    /// Stdin writer for sending requests
    stdin_writer: Arc<Mutex<Option<ChildStdin>>>,

    /// Stdout reader for receiving responses
    stdout_reader: Arc<Mutex<Option<BufReader<ChildStdout>>>>,

    /// Held for one whole request/response round trip. llama-helper answers
    /// one line per request, in order, so two callers interleaving their
    /// write/read halves would read each other's replies. Waiters are served
    /// by priority (see [`Priority`]).
    exchange: Arc<ExchangeQueue>,

    /// Whether the running helper understands request ids and `cancel`. Set
    /// once a reply echoes an id; reset on every spawn.
    cancel_supported: Arc<AtomicBool>,

    /// Held while checking and (re)spawning the helper, so two callers can
    /// never interleave child/stdin/stdout handles or start duplicate loops.
    spawn_lock: Arc<Mutex<()>>,

    /// Bumped on every spawn; health and idle loops of an older helper exit.
    generation: Arc<AtomicUsize>,

    /// Last activity timestamp
    last_activity: Arc<RwLock<Instant>>,

    /// Health status
    is_healthy: Arc<AtomicBool>,

    /// Shutdown flag
    should_shutdown: Arc<AtomicBool>,

    /// Active request count (for graceful shutdown)
    active_request_count: Arc<AtomicUsize>,

    /// Path to llama-helper binary
    helper_binary_path: PathBuf,

    /// Current model path (if loaded)
    current_model_path: Arc<RwLock<Option<PathBuf>>>,

    /// Idle timeout in seconds (configurable via env var)
    idle_timeout_secs: u64,
}

/// RAII guard for tracking active requests
/// Decrements the active request count when dropped
struct RequestGuard {
    counter: Arc<AtomicUsize>,
}

impl RequestGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self { counter }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SidecarManager {
    /// Create a new sidecar manager
    pub fn new(_app_data_dir: PathBuf) -> Result<Self> {
        let helper_binary_path = Self::resolve_helper_binary()?;

        // Get idle timeout from env var or use default
        let idle_timeout_secs = std::env::var("LLAMA_IDLE_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(models::DEFAULT_IDLE_TIMEOUT_SECS);

        log::info!(
            "SidecarManager initialized with idle timeout: {}s",
            idle_timeout_secs
        );
        log::info!("Helper binary path: {}", helper_binary_path.display());

        Ok(Self {
            child_process: Arc::new(Mutex::new(None)),
            stdin_writer: Arc::new(Mutex::new(None)),
            stdout_reader: Arc::new(Mutex::new(None)),
            exchange: Arc::new(ExchangeQueue::default()),
            cancel_supported: Arc::new(AtomicBool::new(false)),
            spawn_lock: Arc::new(Mutex::new(())),
            generation: Arc::new(AtomicUsize::new(0)),
            last_activity: Arc::new(RwLock::new(Instant::now())),
            is_healthy: Arc::new(AtomicBool::new(false)),
            should_shutdown: Arc::new(AtomicBool::new(false)),
            active_request_count: Arc::new(AtomicUsize::new(0)),
            helper_binary_path,
            current_model_path: Arc::new(RwLock::new(None)),
            idle_timeout_secs,
        })
    }

    #[cfg(test)]
    fn new_with_binary(helper_binary_path: PathBuf) -> Self {
        Self {
            child_process: Arc::new(Mutex::new(None)),
            stdin_writer: Arc::new(Mutex::new(None)),
            stdout_reader: Arc::new(Mutex::new(None)),
            exchange: Arc::new(ExchangeQueue::default()),
            cancel_supported: Arc::new(AtomicBool::new(false)),
            spawn_lock: Arc::new(Mutex::new(())),
            generation: Arc::new(AtomicUsize::new(0)),
            last_activity: Arc::new(RwLock::new(Instant::now())),
            is_healthy: Arc::new(AtomicBool::new(false)),
            should_shutdown: Arc::new(AtomicBool::new(false)),
            active_request_count: Arc::new(AtomicUsize::new(0)),
            helper_binary_path,
            current_model_path: Arc::new(RwLock::new(None)),
            idle_timeout_secs: models::DEFAULT_IDLE_TIMEOUT_SECS,
        }
    }

    /// Resolve the path to llama-helper binary
    fn resolve_helper_binary() -> Result<PathBuf> {
        // 1. Check environment variable (dev mode or manual override)
        if let Ok(env_path) = std::env::var("MEETILY_LLAMA_HELPER") {
            if !env_path.is_empty() {
                let path = PathBuf::from(env_path);
                if path.exists() {
                    log::info!(
                        "Using llama-helper from MEETILY_LLAMA_HELPER: {}",
                        path.display()
                    );
                    return Ok(path);
                }
            }
        }

        // In production, Tauri bundles the binary with target triple suffix
        // 2. Check relative to current executable (most reliable for AppImage/bundled apps)
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                log::info!(
                    "Searching for llama-helper relative to executable: {}",
                    exe_dir.display()
                );

                // Get the target triple (same logic as before)
                let target_triple = std::env::var("TARGET").unwrap_or_else(|_| {
                    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
                    {
                        "x86_64-unknown-linux-gnu".to_string()
                    }
                    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
                    {
                        "aarch64-unknown-linux-gnu".to_string()
                    }
                    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
                    {
                        "x86_64-apple-darwin".to_string()
                    }
                    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                    {
                        "aarch64-apple-darwin".to_string()
                    }
                    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
                    {
                        "x86_64-pc-windows-msvc".to_string()
                    }
                    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
                    {
                        "aarch64-pc-windows-msvc".to_string()
                    }
                    #[cfg(not(any(
                        all(
                            target_os = "linux",
                            any(target_arch = "x86_64", target_arch = "aarch64")
                        ),
                        all(
                            target_os = "macos",
                            any(target_arch = "x86_64", target_arch = "aarch64")
                        ),
                        all(
                            target_os = "windows",
                            any(target_arch = "x86_64", target_arch = "aarch64")
                        )
                    )))]
                    {
                        "unknown".to_string()
                    }
                });

                let binary_name = if cfg!(windows) {
                    format!("llama-helper-{}.exe", target_triple)
                } else {
                    format!("llama-helper-{}", target_triple)
                };

                // Try exact match in exe dir
                let bundled = exe_dir.join(&binary_name);
                if bundled.exists() {
                    log::info!(
                        "Found exact match next to executable: {}",
                        bundled.display()
                    );
                    return Ok(bundled);
                }

                // Fuzzy match in exe dir
                log::info!("Attempting fuzzy match in exe dir: {}", exe_dir.display());
                if let Ok(entries) = std::fs::read_dir(exe_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            if name.starts_with("llama-helper") && !name.ends_with(".d") {
                                log::info!(
                                    "Found fuzzy match next to executable: {}",
                                    path.display()
                                );
                                return Ok(path);
                            }
                        }
                    }
                }
            }
        }

        // 3. Check bundled resources (RESOURCE_DIR) - Fallback
        if let Ok(resource_dir) = std::env::var("RESOURCE_DIR") {
            log::info!(
                "Searching for llama-helper in RESOURCE_DIR: {}",
                resource_dir
            );
            let resource_path = PathBuf::from(&resource_dir);
            // Get the target triple again (or we could have shared it, but code duplication is safer for this tool usage)
            let target_triple = std::env::var("TARGET").unwrap_or_else(|_| {
                #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
                {
                    "x86_64-unknown-linux-gnu".to_string()
                }
                // ... (abbreviated for brevity in thought, but must be full in tool)
                #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
                {
                    "aarch64-unknown-linux-gnu".to_string()
                }
                #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
                {
                    "x86_64-apple-darwin".to_string()
                }
                #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                {
                    "aarch64-apple-darwin".to_string()
                }
                #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
                {
                    "x86_64-pc-windows-msvc".to_string()
                }
                #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
                {
                    "aarch64-pc-windows-msvc".to_string()
                }
                #[cfg(not(any(
                    all(
                        target_os = "linux",
                        any(target_arch = "x86_64", target_arch = "aarch64")
                    ),
                    all(
                        target_os = "macos",
                        any(target_arch = "x86_64", target_arch = "aarch64")
                    ),
                    all(
                        target_os = "windows",
                        any(target_arch = "x86_64", target_arch = "aarch64")
                    )
                )))]
                {
                    "unknown".to_string()
                }
            });

            let binary_name = if cfg!(windows) {
                format!("llama-helper-{}.exe", target_triple)
            } else {
                format!("llama-helper-{}", target_triple)
            };

            let bundled = resource_path.join(&binary_name);
            if bundled.exists() {
                log::info!("Found exact match in RESOURCE_DIR: {}", bundled.display());
                return Ok(bundled);
            }

            // Fuzzy match in RESOURCE_DIR
            if let Ok(entries) = std::fs::read_dir(&resource_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("llama-helper") && !name.ends_with(".d") {
                            log::info!("Found fuzzy match in RESOURCE_DIR: {}", path.display());
                            return Ok(path);
                        }
                    }
                }
            }
        } else {
            log::warn!("RESOURCE_DIR environment variable not set");
        }

        // 3. Fallback for dev: try relative paths from workspace (no target triple in dev builds)
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let project_root = PathBuf::from(&manifest_dir)
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| anyhow!("Failed to determine project root"))?
                .to_path_buf();

            let candidates = vec![
                project_root.join("target/release/llama-helper"),
                project_root.join("target/debug/llama-helper"),
                project_root.join("target/release/llama-helper.exe"),
                project_root.join("target/debug/llama-helper.exe"),
            ];

            for candidate in candidates {
                if candidate.exists() {
                    log::info!("Using dev llama-helper: {}", candidate.display());
                    return Ok(candidate);
                }
            }
        }

        Err(anyhow!(
            "llama-helper binary not found. Build with 'cd llama-helper && cargo build --release' or set MEETILY_LLAMA_HELPER env var."
        ))
    }

    /// Ensure sidecar is running, spawn if needed
    ///
    /// A running, healthy helper is never restarted for a different model:
    /// every request names its model and the helper switches models itself,
    /// inside the serialized exchange, so a caller asking for another model
    /// can never kill someone else's request in flight.
    pub async fn ensure_running(&self, model_path: PathBuf) -> Result<()> {
        let _spawning = self.spawn_lock.lock().await;
        if self.is_healthy() {
            log::debug!("Sidecar already running");
            self.update_activity().await;
            return Ok(());
        }
        self.spawn(model_path).await
    }

    /// Spawn the sidecar process
    async fn spawn(&self, model_path: PathBuf) -> Result<()> {
        // Shutdown existing process if running
        self.shutdown().await?;

        log::info!("Spawning llama-helper sidecar");
        log::info!("Model path: {}", model_path.display());

        #[cfg(unix)]
        let mut command = tokio::process::Command::new("nice");

        #[cfg(not(unix))]
        let mut command = tokio::process::Command::new(&self.helper_binary_path);

        #[cfg(unix)]
        command.arg("-n").arg("10").arg(&self.helper_binary_path);

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Log stderr to main process
            // A shutdown future dropped part-way must not leak a running helper.
            .kill_on_drop(true)
            .env("LLAMA_IDLE_TIMEOUT", self.idle_timeout_secs.to_string());

        #[cfg(target_os = "windows")]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;

            command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }

        let mut child = command.spawn().with_context(|| {
            format!(
                "Failed to spawn llama-helper at {:?}",
                self.helper_binary_path
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdout"))?;

        // Store handles
        {
            let mut child_lock = self.child_process.lock().await;
            *child_lock = Some(child);
        }

        {
            let mut stdin_lock = self.stdin_writer.lock().await;
            *stdin_lock = Some(stdin);
        }

        {
            let mut stdout_lock = self.stdout_reader.lock().await;
            *stdout_lock = Some(BufReader::new(stdout));
        }

        // Update state
        {
            let mut current_model = self.current_model_path.write().await;
            *current_model = Some(model_path);
        }

        self.cancel_supported.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.is_healthy.store(true, Ordering::SeqCst);
        self.should_shutdown.store(false, Ordering::SeqCst);
        self.update_activity().await;

        log::info!("Sidecar spawned successfully");

        // Start background tasks
        self.start_health_check_loop();
        self.start_idle_check_loop();

        Ok(())
    }

    /// Send a request to the sidecar and wait for its response, at `Normal`
    /// priority (`Background` for [`OnAbandon::Yield`]) and without an id.
    ///
    /// Requests are serialized: the whole write/read round trip runs under
    /// the exchange queue, and time spent waiting for it does not count
    /// toward `timeout`. Cancelling while still waiting never touches the
    /// sidecar.
    ///
    /// Once the request is written, `on_abandon` decides what a timeout or
    /// cancellation does: [`OnAbandon::StopSidecar`] kills the helper so the
    /// generation stops at once; the other policies return to the caller
    /// immediately and leave the reply to be read and discarded in the
    /// background (up to `ABANDONED_DRAIN_LIMIT`), so other callers sharing
    /// the sidecar are unaffected.
    pub async fn send_request(
        &self,
        request_json: String,
        timeout: Duration,
        cancellation_token: Option<&CancellationToken>,
        on_abandon: OnAbandon,
    ) -> Result<String> {
        self.send(SidecarRequest {
            json: request_json,
            id: None,
            priority: on_abandon.priority(),
            timeout,
            timeout_includes_queue: false,
            cancellation_token,
            on_abandon,
            tokens: None,
        })
        .await
    }

    /// Send one request. See [`SidecarRequest`] for the knobs.
    pub async fn send(&self, request: SidecarRequest<'_>) -> Result<String> {
        let SidecarRequest {
            json: request_json,
            id,
            priority,
            timeout,
            timeout_includes_queue,
            cancellation_token,
            on_abandon,
            tokens,
        } = request;
        let guard = RequestGuard::new(self.active_request_count.clone());
        let _busy = crate::clock::busy();
        let queued_at = Instant::now();
        crate::replay::trace::emit(
            "sidecar",
            || serde_json::json!({ "phase": "queued", "id": id, "priority": format!("{priority:?}") }),
        );
        let acquire = self.acquire_exchange(priority, id);
        let exchange = {
            let queue_limit = async {
                if timeout_includes_queue {
                    tokio::time::sleep(timeout).await
                } else {
                    std::future::pending().await
                }
            };
            let cancelled = async {
                match cancellation_token {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = cancelled => return Err(anyhow!(SidecarError::Cancelled)),
                _ = queue_limit => return Err(anyhow!(SidecarError::Busy(timeout))),
                exchange = acquire => exchange,
            }
        };
        crate::replay::trace::emit("sidecar", || {
            serde_json::json!({
                "phase": "acquired",
                "id": id,
                "priority": format!("{priority:?}"),
                "waited_ms": queued_at.elapsed().as_millis() as u64,
            })
        });
        let timeout = if timeout_includes_queue {
            timeout.saturating_sub(queued_at.elapsed())
        } else {
            timeout
        };

        let pipes = self.pipes();
        let (reply_tx, reply_rx) = oneshot::channel();
        let drain_limit = timeout.max(ABANDONED_DRAIN_LIMIT);
        let round_trip = tokio::spawn(async move {
            let _exchange = exchange;
            let _guard = guard;
            let result = match tokio::time::timeout(
                drain_limit,
                pipes.round_trip(&request_json, tokens.as_ref()),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    // The reply never came: the line protocol is out of
                    // step, so the helper has to go.
                    log::error!(
                        "llama-helper gave no reply within {:?}, stopping it",
                        drain_limit
                    );
                    pipes.kill().await;
                    Err(anyhow!(SidecarError::TimedOut(drain_limit)))
                }
            };
            if result.is_ok() {
                *pipes.last_activity.write().await = Instant::now();
            }
            crate::replay::trace::emit("sidecar", || {
                serde_json::json!({
                    "phase": "done",
                    "id": id,
                    "ok": result.is_ok(),
                    "busy_ms": queued_at.elapsed().as_millis() as u64,
                })
            });
            let _ = reply_tx.send(result);
        });

        let cancelled = async {
            match cancellation_token {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let abandoned = tokio::select! {
            reply = reply_rx => {
                return reply.unwrap_or_else(|_| {
                    Err(anyhow!("Sidecar request task ended without a reply"))
                });
            }
            _ = tokio::time::sleep(timeout) => SidecarError::TimedOut(timeout),
            _ = cancelled => SidecarError::Cancelled,
        };
        match on_abandon {
            OnAbandon::StopSidecar => {
                log::warn!("{abandoned}; shutting down the sidecar to stop generation");
                if let Err(shutdown_err) = self.shutdown().await {
                    log::error!("Failed to shutdown sidecar: {}", shutdown_err);
                }
                // The kill ends the read with EOF, which releases the lock.
                let _ = tokio::time::timeout(Duration::from_secs(5), round_trip).await;
            }
            OnAbandon::LetFinish | OnAbandon::Yield => {
                // Ask the helper to stop early; its one final reply is still
                // read (and dropped) by the round trip, so nothing desyncs.
                match id {
                    Some(id) if self.cancel_supported.load(Ordering::SeqCst) => {
                        log::info!("{abandoned}; cancelling request {id} in the helper");
                        self.pipes().send_cancel(id).await;
                    }
                    _ => log::info!("{abandoned}; leaving the reply to finish in the background"),
                }
            }
        }
        Err(anyhow!(abandoned))
    }

    /// Waits for the exchange at `priority`. An interactive waiter asks an
    /// in-flight background request to stop (once), when the helper supports it.
    async fn acquire_exchange(&self, priority: Priority, id: Option<u64>) -> ExchangeGuard {
        let mut waiting = WaitingGuard::new(self.exchange.clone(), priority);
        loop {
            let released = self.exchange.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            let can_preempt = self.cancel_supported.load(Ordering::SeqCst);
            match self
                .exchange
                .try_acquire(priority, id, can_preempt, &mut waiting)
            {
                Attempt::Acquired(guard) => return guard,
                Attempt::Wait { preempt } => {
                    if let Some(victim) = preempt {
                        crate::replay::trace::emit(
                            "sidecar",
                            || serde_json::json!({ "phase": "preempt", "id": victim, "by": id }),
                        );
                        log::info!("Pre-empting background llama-helper request {victim}");
                        self.pipes().send_cancel(victim).await;
                    }
                }
            }
            released.await;
        }
    }

    fn pipes(&self) -> Pipes {
        Pipes {
            child_process: self.child_process.clone(),
            stdin_writer: self.stdin_writer.clone(),
            stdout_reader: self.stdout_reader.clone(),
            is_healthy: self.is_healthy.clone(),
            current_model_path: self.current_model_path.clone(),
            last_activity: self.last_activity.clone(),
            cancel_supported: self.cancel_supported.clone(),
        }
    }

    /// Read a single line response from stdout
    async fn read_response(&self) -> Result<String> {
        self.pipes().read_line().await
    }

    /// Send ping to keep sidecar alive
    async fn send_ping(&self) -> Result<()> {
        let request = serde_json::json!({"type": "ping"}).to_string();
        let timeout = Duration::from_secs(5);

        // Note: We don't use send_request here to avoid incrementing active_request_count
        // for internal health checks, as that would prevent graceful shutdown.
        // A busy sidecar is alive by definition, so skip the ping rather than
        // queue behind (or interleave with) a real request.
        let Some(_exchange) = self.exchange.try_acquire_idle() else {
            return Ok(());
        };

        // Write request
        {
            let mut stdin_lock = self.stdin_writer.lock().await;
            if let Some(stdin) = stdin_lock.as_mut() {
                stdin.write_all(request.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
            } else {
                return Err(anyhow!("Sidecar not running"));
            }
        }

        // Read response
        let response = tokio::time::timeout(timeout, self.read_response()).await??;

        let resp: serde_json::Value = serde_json::from_str(&response)?;
        if resp.get("type").and_then(|t| t.as_str()) == Some("pong") {
            Ok(())
        } else {
            Err(anyhow!("Unexpected ping response: {}", response))
        }
    }

    /// Gracefully shutdown the sidecar
    /// Waits for active requests to complete before killing the process
    pub async fn shutdown_gracefully(&self) -> Result<()> {
        log::info!("Initiating graceful shutdown of sidecar");

        // Set shutdown flag to prevent new internal tasks
        self.should_shutdown.store(true, Ordering::SeqCst);

        // Wait for active requests to complete
        // We poll every 500ms
        let start = Instant::now();
        let max_wait = Duration::from_secs(600); // Wait up to 10 minutes for long generations

        loop {
            let count = self.active_request_count.load(Ordering::SeqCst);
            if count == 0 {
                log::info!("No active requests, proceeding with shutdown");
                break;
            }

            if start.elapsed() > max_wait {
                log::warn!(
                    "Timed out waiting for active requests ({} active), forcing shutdown",
                    count
                );
                break;
            }

            log::debug!("Waiting for {} active requests to complete...", count);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        self.shutdown().await
    }

    /// Force shutdown the sidecar
    pub async fn shutdown(&self) -> Result<()> {
        // Set shutdown flag
        self.should_shutdown.store(true, Ordering::SeqCst);

        // Send shutdown command
        if self.is_healthy() {
            let request = serde_json::json!({"type": "shutdown"}).to_string();
            let _timeout = Duration::from_secs(5);

            // Try to send shutdown command, but ignore errors
            // We don't use send_request to avoid incrementing counter
            let _ = async {
                let mut stdin_lock = self.stdin_writer.lock().await;
                if let Some(stdin) = stdin_lock.as_mut() {
                    stdin.write_all(request.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await?;
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
        }

        // Detach the process before awaiting its exit, so a caller dropping
        // this future part-way (a select!, an abort) cannot leave the manager
        // half shut down: it is already unhealthy with no model, so the next
        // request respawns, and the child is `kill_on_drop`, so dropping it
        // mid-wait still ends the process. stdout is cleared only after the
        // process is gone: a request mid-read holds its lock until EOF.
        self.is_healthy.store(false, Ordering::SeqCst);
        let child = self.child_process.lock().await.take();
        *self.stdin_writer.lock().await = None;
        *self.current_model_path.write().await = None;

        if let Some(mut child) = child {
            match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => {
                    log::info!("Sidecar exited with status: {}", status);
                }
                Ok(Err(e)) => {
                    log::error!("Failed to wait for sidecar: {}", e);
                }
                Err(_) => {
                    log::warn!("Sidecar didn't exit gracefully, killing");
                    let _ = child.kill().await;
                }
            }
        }
        *self.stdout_reader.lock().await = None;

        log::info!("Sidecar shutdown complete");
        Ok(())
    }

    /// Check if sidecar is healthy
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::SeqCst)
    }

    /// Update last activity timestamp
    async fn update_activity(&self) {
        let mut last_activity = self.last_activity.write().await;
        *last_activity = Instant::now();
    }

    /// Get seconds since last activity
    async fn seconds_since_activity(&self) -> u64 {
        let last_activity = self.last_activity.read().await;
        last_activity.elapsed().as_secs()
    }

    /// Start health check loop (runs in background)
    fn start_health_check_loop(&self) {
        let generation = self.generation.load(Ordering::SeqCst);
        let manager = Self {
            child_process: self.child_process.clone(),
            stdin_writer: self.stdin_writer.clone(),
            stdout_reader: self.stdout_reader.clone(),
            exchange: self.exchange.clone(),
            cancel_supported: self.cancel_supported.clone(),
            spawn_lock: self.spawn_lock.clone(),
            generation: self.generation.clone(),
            last_activity: self.last_activity.clone(),
            is_healthy: self.is_healthy.clone(),
            should_shutdown: self.should_shutdown.clone(),
            active_request_count: self.active_request_count.clone(),
            helper_binary_path: self.helper_binary_path.clone(),
            current_model_path: self.current_model_path.clone(),
            idle_timeout_secs: self.idle_timeout_secs,
        };

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if manager.should_shutdown.load(Ordering::SeqCst)
                    || manager.generation.load(Ordering::SeqCst) != generation
                {
                    log::debug!("Health check loop: shutdown flag set, exiting");
                    break;
                }

                if !manager.is_healthy() {
                    log::debug!("Health check loop: sidecar unhealthy, skipping ping");
                    continue;
                }

                // Don't ping if we are busy with a request
                if manager.active_request_count.load(Ordering::SeqCst) > 0 {
                    continue;
                }

                log::debug!("Health check: sending ping");
                if let Err(e) = manager.send_ping().await {
                    log::warn!("Health check failed: {}", e);
                    manager.is_healthy.store(false, Ordering::SeqCst);
                }
            }

            log::debug!("Health check loop exited");
        });
    }

    /// Start idle check loop (runs in background)
    fn start_idle_check_loop(&self) {
        let generation = self.generation.load(Ordering::SeqCst);
        let manager = Self {
            child_process: self.child_process.clone(),
            stdin_writer: self.stdin_writer.clone(),
            stdout_reader: self.stdout_reader.clone(),
            exchange: self.exchange.clone(),
            cancel_supported: self.cancel_supported.clone(),
            spawn_lock: self.spawn_lock.clone(),
            generation: self.generation.clone(),
            last_activity: self.last_activity.clone(),
            is_healthy: self.is_healthy.clone(),
            should_shutdown: self.should_shutdown.clone(),
            active_request_count: self.active_request_count.clone(),
            helper_binary_path: self.helper_binary_path.clone(),
            current_model_path: self.current_model_path.clone(),
            idle_timeout_secs: self.idle_timeout_secs,
        };

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if manager.should_shutdown.load(Ordering::SeqCst)
                    || manager.generation.load(Ordering::SeqCst) != generation
                {
                    log::debug!("Idle check loop: shutdown flag set, exiting");
                    break;
                }

                // Don't shutdown if we are busy
                if manager.active_request_count.load(Ordering::SeqCst) > 0 {
                    // Update activity to prevent timeout immediately after request finishes
                    manager.update_activity().await;
                    continue;
                }

                let idle_secs = manager.seconds_since_activity().await;
                log::debug!("Idle check: {}s since last activity", idle_secs);

                if idle_secs > manager.idle_timeout_secs {
                    log::info!(
                        "Sidecar idle for {}s (timeout: {}s), shutting down",
                        idle_secs,
                        manager.idle_timeout_secs
                    );

                    if let Err(e) = manager.shutdown().await {
                        log::error!("Failed to shutdown idle sidecar: {}", e);
                    }

                    break;
                }
            }

            log::debug!("Idle check loop exited");
        });
    }
}

/// What a timed-out or cancelled request does to the sidecar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnAbandon {
    /// Kill the helper so the generation stops immediately (a user cancel).
    #[default]
    StopSidecar,
    /// Leave the helper running; the request is cancelled inside the helper
    /// when it supports that, and its reply is read and dropped in the
    /// background. For callers that share the sidecar and must never
    /// interrupt others (Copilot).
    LetFinish,
    /// Like `LetFinish`, and the request runs at `Background` priority, so
    /// Copilot may pre-empt it. For the live timeline.
    Yield,
}

impl OnAbandon {
    /// The queue priority a plain `send_request` with this policy gets.
    pub fn priority(self) -> Priority {
        match self {
            OnAbandon::Yield => Priority::Background,
            OnAbandon::StopSidecar | OnAbandon::LetFinish => Priority::Normal,
        }
    }
}

/// Who goes first when several callers wait for the sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// The live timeline: waits behind everyone, and an in-flight request
    /// can be pre-empted by an `Interactive` one.
    Background,
    /// Summaries, titles, chat, token counts: never pre-empted.
    Normal,
    /// Copilot: served first.
    Interactive,
}

impl Priority {
    fn index(self) -> usize {
        match self {
            Priority::Background => 0,
            Priority::Normal => 1,
            Priority::Interactive => 2,
        }
    }
}

/// One request to the sidecar.
pub struct SidecarRequest<'a> {
    pub json: String,
    /// The request's `id` field, needed to cancel it in the helper.
    pub id: Option<u64>,
    pub priority: Priority,
    pub timeout: Duration,
    /// Count time waiting for the sidecar toward `timeout` (Copilot: the user
    /// is waiting either way). A queue timeout never touches the sidecar.
    pub timeout_includes_queue: bool,
    pub cancellation_token: Option<&'a CancellationToken>,
    pub on_abandon: OnAbandon,
    /// Receives the text of each streamed `token` line.
    pub tokens: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

/// An interactive request pre-empts background (timeline) work at most this
/// often; in between it waits for the timeline request to finish, so the
/// live summary always gets an entry in.
pub const PREEMPT_MIN_GAP: Duration = Duration::from_secs(120);

/// Longest an abandoned request may keep the sidecar busy before the helper is
/// stopped to get the protocol back in step.
pub const ABANDONED_DRAIN_LIMIT: Duration = Duration::from_secs(models::GENERATION_TIMEOUT_SECS);

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("Generation cancelled by user")]
    Cancelled,
    #[error("Request timed out after {0:?}")]
    TimedOut(Duration),
    /// Waited `timeout` for the sidecar without getting it.
    #[error("The built-in model was busy for {0:?}")]
    Busy(Duration),
    /// An interactive request (Copilot) stopped this background one.
    #[error("Generation preempted by an interactive request")]
    Preempted,
}

// ============================================================================
// Exchange queue
// ============================================================================

#[derive(Debug, Clone, Copy)]
struct Holder {
    priority: Priority,
    id: Option<u64>,
    preempt_sent: bool,
}

#[derive(Debug, Default)]
struct QueueState {
    /// When an interactive request last pre-empted a background one.
    last_preempt: Option<Instant>,
    busy: bool,
    /// Waiters per priority (indexed by `Priority::index`).
    waiting: [usize; 3],
    holder: Option<Holder>,
}

/// A lock over the sidecar's request/response exchange that serves waiters by
/// priority. Within one priority, order is whoever wakes first.
#[derive(Debug, Default)]
pub(crate) struct ExchangeQueue {
    state: std::sync::Mutex<QueueState>,
    released: tokio::sync::Notify,
}

/// Holds the exchange; dropping it lets the next waiter in.
pub(crate) struct ExchangeGuard {
    queue: Arc<ExchangeQueue>,
}

impl Drop for ExchangeGuard {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
        state.busy = false;
        state.holder = None;
        drop(state);
        self.queue.released.notify_waiters();
    }
}

/// Counts a caller as waiting until it gets the exchange or gives up.
struct WaitingGuard {
    queue: Arc<ExchangeQueue>,
    priority: Priority,
    registered: bool,
}

impl WaitingGuard {
    fn new(queue: Arc<ExchangeQueue>, priority: Priority) -> Self {
        queue
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .waiting[priority.index()] += 1;
        Self {
            queue,
            priority,
            registered: true,
        }
    }
}

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        if self.registered {
            let mut state = self.queue.state.lock().unwrap_or_else(|e| e.into_inner());
            state.waiting[self.priority.index()] -= 1;
            drop(state);
            // A lower-priority waiter may now be first in line.
            self.queue.released.notify_waiters();
        }
    }
}

enum Attempt {
    Acquired(ExchangeGuard),
    /// Keep waiting; first ask this background request to stop.
    Wait {
        preempt: Option<u64>,
    },
}

impl ExchangeQueue {
    fn try_acquire(
        self: &Arc<Self>,
        priority: Priority,
        id: Option<u64>,
        can_preempt: bool,
        waiting: &mut WaitingGuard,
    ) -> Attempt {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let outranked = state.waiting[priority.index() + 1..]
            .iter()
            .any(|count| *count > 0);
        if !state.busy && !outranked {
            state.busy = true;
            state.holder = Some(Holder {
                priority,
                id,
                preempt_sent: false,
            });
            if waiting.registered {
                state.waiting[priority.index()] -= 1;
                waiting.registered = false;
            }
            return Attempt::Acquired(ExchangeGuard {
                queue: self.clone(),
            });
        }
        // Meeting time (`crate::clock`), which a replay runs faster than real.
        let now = crate::clock::now().into_std();
        let recently = state
            .last_preempt
            .is_some_and(|at| now.saturating_duration_since(at) < PREEMPT_MIN_GAP);
        let preempt = match state.holder.as_mut() {
            Some(holder)
                if priority == Priority::Interactive
                    && can_preempt
                    && !recently
                    && holder.priority == Priority::Background
                    && !holder.preempt_sent =>
            {
                holder.preempt_sent = holder.id.is_some();
                holder.id
            }
            _ => None,
        };
        if preempt.is_some() {
            state.last_preempt = Some(now);
        }
        Attempt::Wait { preempt }
    }

    /// The exchange if nobody holds or wants it (health pings).
    pub(crate) fn try_acquire_idle(self: &Arc<Self>) -> Option<ExchangeGuard> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.busy || state.waiting.iter().any(|count| *count > 0) {
            return None;
        }
        state.busy = true;
        state.holder = None;
        Some(ExchangeGuard {
            queue: self.clone(),
        })
    }
}

/// The handles one request needs, owned, so its round trip can outlive the
/// caller (see [`OnAbandon::LetFinish`]).
struct Pipes {
    child_process: Arc<Mutex<Option<Child>>>,
    stdin_writer: Arc<Mutex<Option<ChildStdin>>>,
    stdout_reader: Arc<Mutex<Option<BufReader<ChildStdout>>>>,
    is_healthy: Arc<AtomicBool>,
    current_model_path: Arc<RwLock<Option<PathBuf>>>,
    last_activity: Arc<RwLock<Instant>>,
    cancel_supported: Arc<AtomicBool>,
}

#[derive(serde::Deserialize)]
struct TokenLine {
    #[serde(default)]
    text: String,
}

/// Whether a reply line is a streamed token rather than a request's final reply.
fn is_token_line(line: &str) -> bool {
    line.starts_with(r#"{"type":"token""#)
}

impl Pipes {
    async fn write_line(&self, line: &str) -> Result<()> {
        let mut stdin_lock = self.stdin_writer.lock().await;
        let stdin = stdin_lock
            .as_mut()
            .ok_or_else(|| anyhow!("Sidecar not running"))?;
        stdin
            .write_all(line.as_bytes())
            .await
            .context("Failed to write request to stdin")?;
        stdin
            .write_all(b"\n")
            .await
            .context("Failed to write newline")?;
        stdin.flush().await.context("Failed to flush stdin")?;
        Ok(())
    }

    /// Writes the request and reads until its final reply, forwarding any
    /// streamed token lines on the way.
    async fn round_trip(
        &self,
        request_json: &str,
        tokens: Option<&tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Result<String> {
        self.write_line(request_json).await?;
        loop {
            let line = self.read_line().await?;
            if is_token_line(&line) {
                if let (Some(tokens), Ok(token)) =
                    (tokens, serde_json::from_str::<TokenLine>(&line))
                {
                    let _ = tokens.send(token.text);
                }
                continue;
            }
            if serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .is_some_and(|reply| reply.get("id").is_some_and(|id| !id.is_null()))
            {
                self.cancel_supported.store(true, Ordering::SeqCst);
            }
            return Ok(line);
        }
    }

    /// Asks the helper to stop request `id`. It still writes that request's
    /// one final reply, which its own round trip reads. Never answered.
    async fn send_cancel(&self, id: u64) {
        let line = serde_json::json!({"type": "cancel", "id": id}).to_string();
        if let Err(error) = self.write_line(&line).await {
            log::warn!("Could not cancel llama-helper request {id}: {error:#}");
        }
    }

    async fn read_line(&self) -> Result<String> {
        let mut stdout_lock = self.stdout_reader.lock().await;
        let reader = stdout_lock
            .as_mut()
            .ok_or_else(|| anyhow!("Sidecar not running"))?;
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .context("Failed to read response from stdout")?;
        if line.is_empty() {
            return Err(anyhow!("Sidecar closed stdout (process may have crashed)"));
        }
        Ok(line.trim().to_string())
    }

    /// Stop the helper without the polite shutdown request.
    async fn kill(&self) {
        self.is_healthy.store(false, Ordering::SeqCst);
        let child = self.child_process.lock().await.take();
        *self.stdin_writer.lock().await = None;
        *self.current_model_path.write().await = None;
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
        *self.stdout_reader.lock().await = None;
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        // Set shutdown flag
        self.should_shutdown.store(true, Ordering::SeqCst);

        // Note: Actual cleanup happens in shutdown() method
        // We can't do async work in Drop, so this is best-effort
        log::debug!("SidecarManager dropped");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for llama-helper that echoes each request line back, after a
    /// second for requests that contain "slow".
    fn fake_helper(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("fake-llama-helper");
        std::fs::write(
            &path,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in *slow*) sleep 1;; esac\n  printf '%s\\n' \"$line\"\ndone\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn running_fake() -> (tempfile::TempDir, Arc<SidecarManager>) {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_helper(dir.path());
        let manager = Arc::new(SidecarManager::new_with_binary(binary));
        manager
            .ensure_running(dir.path().join("model.gguf"))
            .await
            .unwrap();
        (dir, manager)
    }

    fn request(tag: &str) -> String {
        format!(r#"{{"type":"generate","prompt":"{tag}"}}"#)
    }

    fn is_error(error: &anyhow::Error, cancelled: bool) -> bool {
        match error.downcast_ref::<SidecarError>() {
            Some(SidecarError::Cancelled) => cancelled,
            Some(SidecarError::TimedOut(_)) => !cancelled,
            Some(SidecarError::Busy(_) | SidecarError::Preempted) | None => false,
        }
    }

    #[tokio::test]
    async fn concurrent_requests_each_get_their_own_reply() {
        let (_dir, manager) = running_fake().await;
        let mut tasks = Vec::new();
        for i in 0..9 {
            let manager = manager.clone();
            let tag = if i % 4 == 0 {
                format!("slow-{i}")
            } else {
                format!("fast-{i}")
            };
            tasks.push(tokio::spawn(async move {
                let reply = manager
                    .send_request(
                        request(&tag),
                        Duration::from_secs(5),
                        None,
                        OnAbandon::StopSidecar,
                    )
                    .await
                    .unwrap();
                (tag, reply)
            }));
        }
        for task in tasks {
            let (tag, reply) = task.await.unwrap();
            assert_eq!(reply, request(&tag));
        }
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_abandoned_request_leaves_the_sidecar_running_and_its_reply_unread() {
        let (_dir, manager) = running_fake().await;
        let error = manager
            .send_request(
                request("slow-abandoned"),
                Duration::from_millis(100),
                None,
                OnAbandon::LetFinish,
            )
            .await
            .unwrap_err();
        assert!(is_error(&error, false), "{error:#}");
        assert!(manager.is_healthy(), "a timeline timeout must not stop it");

        // The next caller waits for the stale reply to drain (that wait is
        // not part of its own 500 ms timeout) and then reads its own reply.
        let reply = manager
            .send_request(
                request("fast-next"),
                Duration::from_millis(500),
                None,
                OnAbandon::StopSidecar,
            )
            .await
            .unwrap();
        assert_eq!(reply, request("fast-next"));
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_while_queued_does_not_touch_the_sidecar() {
        let (_dir, manager) = running_fake().await;
        let busy = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .send_request(
                        request("slow-busy"),
                        Duration::from_secs(5),
                        None,
                        OnAbandon::StopSidecar,
                    )
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let token = CancellationToken::new();
        token.cancel();
        let error = manager
            .send_request(
                request("fast-cancelled"),
                Duration::from_secs(5),
                Some(&token),
                OnAbandon::StopSidecar,
            )
            .await
            .unwrap_err();
        assert!(is_error(&error, true), "{error:#}");
        assert_eq!(busy.await.unwrap().unwrap(), request("slow-busy"));
        assert!(manager.is_healthy());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_user_cancel_stops_the_sidecar() {
        let (_dir, manager) = running_fake().await;
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let error = manager
            .send_request(
                request("slow-cancel"),
                Duration::from_secs(5),
                Some(&token),
                OnAbandon::StopSidecar,
            )
            .await
            .unwrap_err();
        assert!(is_error(&error, true), "{error:#}");
        assert!(!manager.is_healthy());
    }

    #[tokio::test]
    async fn a_dropped_shutdown_leaves_a_restartable_manager() {
        let (dir, manager) = running_fake().await;
        // Drop shutdown() part-way, as a select! or an abort would.
        let _ = tokio::time::timeout(Duration::from_millis(1), manager.shutdown()).await;
        assert!(!manager.is_healthy());
        manager
            .ensure_running(dir.path().join("model.gguf"))
            .await
            .unwrap();
        let reply = manager
            .send_request(
                request("fast-after"),
                Duration::from_secs(5),
                None,
                OnAbandon::StopSidecar,
            )
            .await
            .unwrap();
        assert_eq!(reply, request("fast-after"));
        manager.shutdown().await.unwrap();
    }

    // ------------------------------------------------------------------
    // Priority, pre-emption and streaming, against a helper that speaks the
    // id/cancel/stream protocol.
    // ------------------------------------------------------------------

    /// Answers `generate` like llama-helper: streams one token per word when
    /// asked, takes ~1.5 s for prompts containing "slow" unless cancelled,
    /// and always writes exactly one final reply carrying the request id.
    fn protocol_helper(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("protocol-llama-helper");
        std::fs::write(
            &path,
            r#"#!/usr/bin/env python3
import json, sys, threading, time, queue
cancelled = set()
requests = queue.Queue()
def reader():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if msg.get("type") == "cancel":
            cancelled.add(msg["id"])
            continue
        requests.put(msg)
    requests.put(None)
threading.Thread(target=reader, daemon=True).start()
def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()
while True:
    msg = requests.get()
    if msg is None:
        break
    rid = msg.get("id")
    prompt = msg.get("prompt", "")
    finish = "stop"
    if "slow" in prompt:
        for _ in range(150):
            if rid in cancelled:
                finish = "cancelled"
                break
            time.sleep(0.01)
    if msg.get("stream") and finish == "stop":
        for word in prompt.split(" "):
            send({"type": "token", "id": rid, "text": word + " "})
    send({"type": "response", "text": prompt, "error": None, "finish_reason": finish, "id": rid})
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn running_protocol_helper() -> (tempfile::TempDir, Arc<SidecarManager>) {
        let dir = tempfile::tempdir().unwrap();
        let binary = protocol_helper(dir.path());
        let manager = Arc::new(SidecarManager::new_with_binary(binary));
        manager
            .ensure_running(dir.path().join("model.gguf"))
            .await
            .unwrap();
        // The first reply that echoes an id unlocks cancellation.
        let warmup = manager
            .send(spec(
                generate_line(0, "warmup", false),
                Some(0),
                Priority::Normal,
            ))
            .await
            .unwrap();
        assert!(warmup.contains("warmup"));
        assert!(manager.cancel_supported.load(Ordering::SeqCst));
        (dir, manager)
    }

    fn generate_line(id: u64, prompt: &str, stream: bool) -> String {
        serde_json::json!({"type": "generate", "prompt": prompt, "id": id, "stream": stream})
            .to_string()
    }

    fn spec(json: String, id: Option<u64>, priority: Priority) -> SidecarRequest<'static> {
        SidecarRequest {
            json,
            id,
            priority,
            timeout: Duration::from_secs(10),
            timeout_includes_queue: false,
            cancellation_token: None,
            on_abandon: if priority == Priority::Background {
                OnAbandon::Yield
            } else {
                OnAbandon::LetFinish
            },
            tokens: None,
        }
    }

    fn reply_field(reply: &str, field: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(reply).unwrap()[field].clone()
    }

    #[tokio::test]
    async fn an_interactive_request_preempts_an_in_flight_background_one() {
        let (_dir, manager) = running_protocol_helper().await;
        let background = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .send(spec(
                        generate_line(1, "slow timeline", false),
                        Some(1),
                        Priority::Background,
                    ))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        let started = Instant::now();
        let interactive = manager
            .send(spec(
                generate_line(2, "copilot", false),
                Some(2),
                Priority::Interactive,
            ))
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "copilot waited {:?} behind the timeline",
            started.elapsed()
        );
        assert_eq!(reply_field(&interactive, "id"), 2);
        assert_eq!(reply_field(&interactive, "text"), "copilot");
        // The timeline reads its own (cancelled) reply, never copilot's.
        let background = background.await.unwrap().unwrap();
        assert_eq!(reply_field(&background, "id"), 1);
        assert_eq!(reply_field(&background, "finish_reason"), "cancelled");
        assert!(
            manager.is_healthy(),
            "pre-emption must not stop the sidecar"
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_interactive_request_never_preempts_a_summary() {
        let (_dir, manager) = running_protocol_helper().await;
        let summary = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .send(spec(
                        generate_line(1, "slow summary", false),
                        Some(1),
                        Priority::Normal,
                    ))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        let interactive = manager
            .send(spec(
                generate_line(2, "copilot", false),
                Some(2),
                Priority::Interactive,
            ))
            .await
            .unwrap();
        assert_eq!(reply_field(&interactive, "id"), 2);
        let summary = summary.await.unwrap().unwrap();
        assert_eq!(reply_field(&summary, "finish_reason"), "stop");
        assert_eq!(reply_field(&summary, "text"), "slow summary");
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_interactive_request_jumps_queued_background_work() {
        let (_dir, manager) = running_protocol_helper().await;
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let run = |id: u64, prompt: &'static str, priority: Priority| {
            let manager = manager.clone();
            let order = order.clone();
            tokio::spawn(async move {
                let reply = manager
                    .send(spec(generate_line(id, prompt, false), Some(id), priority))
                    .await
                    .unwrap();
                order.lock().unwrap().push(id);
                reply
            })
        };
        let summary = run(1, "slow summary", Priority::Normal);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let timeline = run(2, "timeline", Priority::Background);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let copilot = run(3, "copilot", Priority::Interactive);
        for task in [summary, timeline, copilot] {
            task.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![1, 3, 2]);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn streamed_tokens_are_forwarded_and_the_final_reply_returned() {
        let (_dir, manager) = running_protocol_helper().await;
        let (tokens, mut received) = tokio::sync::mpsc::unbounded_channel();
        let mut request = spec(
            generate_line(5, "hello there friend", true),
            Some(5),
            Priority::Interactive,
        );
        request.tokens = Some(tokens);
        let reply = manager.send(request).await.unwrap();
        assert_eq!(reply_field(&reply, "type"), "response");
        let mut streamed = String::new();
        while let Ok(token) = received.try_recv() {
            streamed.push_str(&token);
        }
        assert_eq!(streamed, "hello there friend ");
        // The next request is not confused by the token lines.
        let next = manager
            .send(spec(
                generate_line(6, "next", false),
                Some(6),
                Priority::Normal,
            ))
            .await
            .unwrap();
        assert_eq!(reply_field(&next, "id"), 6);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_abandoned_copilot_request_is_cancelled_in_the_helper_not_killed() {
        let (_dir, manager) = running_protocol_helper().await;
        let mut request = spec(
            generate_line(7, "slow copilot", false),
            Some(7),
            Priority::Interactive,
        );
        request.timeout = Duration::from_millis(100);
        let error = manager.send(request).await.unwrap_err();
        assert!(is_error(&error, false), "{error:#}");
        assert!(manager.is_healthy());
        let started = Instant::now();
        let next = manager
            .send(spec(
                generate_line(8, "after", false),
                Some(8),
                Priority::Normal,
            ))
            .await
            .unwrap();
        assert_eq!(reply_field(&next, "id"), 8);
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "the cancel should end the abandoned generation early"
        );
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_queue_timeout_never_touches_the_sidecar() {
        let (_dir, manager) = running_protocol_helper().await;
        let summary = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .send(spec(
                        generate_line(1, "slow summary", false),
                        Some(1),
                        Priority::Normal,
                    ))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut request = spec(
            generate_line(2, "copilot", false),
            Some(2),
            Priority::Interactive,
        );
        request.timeout = Duration::from_millis(200);
        request.timeout_includes_queue = true;
        let error = manager.send(request).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SidecarError>(),
            Some(SidecarError::Busy(_))
        ));
        let summary = summary.await.unwrap().unwrap();
        assert_eq!(reply_field(&summary, "finish_reason"), "stop");
        assert!(manager.is_healthy());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn another_model_never_restarts_a_healthy_helper_under_a_request() {
        // Regression: ensure_running respawned on a model change, so a
        // timeline tick on another model killed an in-flight Copilot stream.
        let (dir, manager) = running_protocol_helper().await;
        let copilot = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .send(spec(
                        generate_line(1, "slow copilot", false),
                        Some(1),
                        Priority::Interactive,
                    ))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        manager
            .ensure_running(dir.path().join("other-model.gguf"))
            .await
            .unwrap();
        let reply = copilot.await.unwrap().unwrap();
        assert_eq!(reply_field(&reply, "finish_reason"), "stop");
        assert_eq!(reply_field(&reply, "text"), "slow copilot");
        assert!(manager.is_healthy());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_starts_spawn_one_helper() {
        let dir = tempfile::tempdir().unwrap();
        let binary = protocol_helper(dir.path());
        let manager = Arc::new(SidecarManager::new_with_binary(binary));
        let mut starts = Vec::new();
        for _ in 0..6 {
            let manager = manager.clone();
            let model = dir.path().join("model.gguf");
            starts.push(tokio::spawn(
                async move { manager.ensure_running(model).await },
            ));
        }
        for start in starts {
            start.await.unwrap().unwrap();
        }
        assert_eq!(manager.generation.load(Ordering::SeqCst), 1, "spawned once");
        let reply = manager
            .send(spec(
                generate_line(3, "hello", false),
                Some(3),
                Priority::Normal,
            ))
            .await
            .unwrap();
        assert_eq!(reply_field(&reply, "id"), 3);
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn the_timeline_is_preempted_at_most_once_per_gap() {
        let (_dir, manager) = running_protocol_helper().await;
        for (round, (timeline_id, copilot_id)) in [(1, 2), (3, 4)].into_iter().enumerate() {
            let background = {
                let manager = manager.clone();
                tokio::spawn(async move {
                    manager
                        .send(spec(
                            generate_line(timeline_id, "slow timeline", false),
                            Some(timeline_id),
                            Priority::Background,
                        ))
                        .await
                })
            };
            tokio::time::sleep(Duration::from_millis(150)).await;
            manager
                .send(spec(
                    generate_line(copilot_id, "copilot", false),
                    Some(copilot_id),
                    Priority::Interactive,
                ))
                .await
                .unwrap();
            let background = background.await.unwrap().unwrap();
            let expected = if round == 0 { "cancelled" } else { "stop" };
            assert_eq!(
                reply_field(&background, "finish_reason"),
                expected,
                "round {round}: the second timeline request in the gap must finish"
            );
        }
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn proactive_copilot_work_never_preempts_the_timeline() {
        let (_dir, manager) = running_protocol_helper().await;
        let background = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .send(spec(
                        generate_line(1, "slow timeline", false),
                        Some(1),
                        Priority::Background,
                    ))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Suggestion confirms and drafts run at Normal priority.
        manager
            .send(spec(
                generate_line(2, "confirm", false),
                Some(2),
                Priority::Normal,
            ))
            .await
            .unwrap();
        let background = background.await.unwrap().unwrap();
        assert_eq!(reply_field(&background, "finish_reason"), "stop");
        manager.shutdown().await.unwrap();
    }
}
