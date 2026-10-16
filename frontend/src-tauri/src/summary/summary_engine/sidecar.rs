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
    /// write/read halves would read each other's replies.
    exchange_lock: Arc<Mutex<()>>,

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
            exchange_lock: Arc::new(Mutex::new(())),
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
            exchange_lock: Arc::new(Mutex::new(())),
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
    pub async fn ensure_running(&self, model_path: PathBuf) -> Result<()> {
        // Check if already running with correct model
        {
            let current_model = self.current_model_path.read().await;
            if current_model.as_ref() == Some(&model_path) && self.is_healthy() {
                log::debug!("Sidecar already running with correct model");
                self.update_activity().await;
                return Ok(());
            }
        }

        // Need to spawn or restart
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

        self.is_healthy.store(true, Ordering::SeqCst);
        self.should_shutdown.store(false, Ordering::SeqCst);
        self.update_activity().await;

        log::info!("Sidecar spawned successfully");

        // Start background tasks
        self.start_health_check_loop();
        self.start_idle_check_loop();

        Ok(())
    }

    /// Send a request to the sidecar and wait for its response.
    ///
    /// Requests are serialized: the whole write/read round trip runs under
    /// `exchange_lock`, and time spent waiting for that lock does not count
    /// toward `timeout`. Cancelling while still waiting for the lock never
    /// touches the sidecar.
    ///
    /// Once the request is written, `on_abandon` decides what a timeout or
    /// cancellation does: [`OnAbandon::StopSidecar`] kills the helper so the
    /// generation stops at once; [`OnAbandon::LetFinish`] returns to the caller
    /// immediately and leaves the reply to be read and discarded in the
    /// background (up to `ABANDONED_DRAIN_LIMIT`), so other callers sharing
    /// the sidecar are unaffected.
    pub async fn send_request(
        &self,
        request_json: String,
        timeout: Duration,
        cancellation_token: Option<&CancellationToken>,
        on_abandon: OnAbandon,
    ) -> Result<String> {
        let guard = RequestGuard::new(self.active_request_count.clone());
        let exchange = {
            let lock = self.exchange_lock.clone().lock_owned();
            match cancellation_token {
                Some(token) => tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(anyhow!(SidecarError::Cancelled)),
                    exchange = lock => exchange,
                },
                None => lock.await,
            }
        };

        let pipes = self.pipes();
        let (reply_tx, reply_rx) = oneshot::channel();
        let drain_limit = timeout.max(ABANDONED_DRAIN_LIMIT);
        let round_trip = tokio::spawn(async move {
            let _exchange = exchange;
            let _guard = guard;
            let result =
                match tokio::time::timeout(drain_limit, pipes.round_trip(&request_json)).await {
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
            OnAbandon::LetFinish => {
                log::info!("{abandoned}; leaving the reply to finish in the background");
            }
        }
        Err(anyhow!(abandoned))
    }

    fn pipes(&self) -> Pipes {
        Pipes {
            child_process: self.child_process.clone(),
            stdin_writer: self.stdin_writer.clone(),
            stdout_reader: self.stdout_reader.clone(),
            is_healthy: self.is_healthy.clone(),
            current_model_path: self.current_model_path.clone(),
            last_activity: self.last_activity.clone(),
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
        let Ok(_exchange) = self.exchange_lock.try_lock() else {
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
        let manager = Self {
            child_process: self.child_process.clone(),
            stdin_writer: self.stdin_writer.clone(),
            stdout_reader: self.stdout_reader.clone(),
            exchange_lock: self.exchange_lock.clone(),
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

                if manager.should_shutdown.load(Ordering::SeqCst) {
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
        let manager = Self {
            child_process: self.child_process.clone(),
            stdin_writer: self.stdin_writer.clone(),
            stdout_reader: self.stdout_reader.clone(),
            exchange_lock: self.exchange_lock.clone(),
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

                if manager.should_shutdown.load(Ordering::SeqCst) {
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
    /// Leave the helper running; its reply is read and dropped in the
    /// background. For background callers (the live timeline) that share the
    /// sidecar with the summary and must never interrupt it.
    LetFinish,
}

/// Longest an abandoned request may keep the sidecar busy before the helper is
/// stopped to get the protocol back in step.
pub const ABANDONED_DRAIN_LIMIT: Duration = Duration::from_secs(models::GENERATION_TIMEOUT_SECS);

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("Generation cancelled by user")]
    Cancelled,
    #[error("Request timed out after {0:?}")]
    TimedOut(Duration),
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
}

impl Pipes {
    async fn round_trip(&self, request_json: &str) -> Result<String> {
        {
            let mut stdin_lock = self.stdin_writer.lock().await;
            let stdin = stdin_lock
                .as_mut()
                .ok_or_else(|| anyhow!("Sidecar not running"))?;
            stdin
                .write_all(request_json.as_bytes())
                .await
                .context("Failed to write request to stdin")?;
            stdin
                .write_all(b"\n")
                .await
                .context("Failed to write newline")?;
            stdin.flush().await.context("Failed to flush stdin")?;
        }
        self.read_line().await
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
            None => false,
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
}
