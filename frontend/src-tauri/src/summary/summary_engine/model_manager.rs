// Model manager for built-in AI models - handles downloads and lifecycle
// Follows the same pattern as whisper_engine/whisper_engine.rs for consistency

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::download_eta::DownloadEta;

use super::models::{get_available_models, get_model_by_name};

// ============================================================================
// Model Status Types
// ============================================================================

/// Detailed download progress info (MB-based with speed)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Bytes downloaded so far
    pub downloaded_bytes: u64,
    /// Total file size in bytes
    pub total_bytes: u64,
    /// Downloaded in MB (for display)
    pub downloaded_mb: f64,
    /// Total size in MB (for display)
    pub total_mb: f64,
    /// Download speed in MB/s
    pub speed_mbps: f64,
    /// Percentage complete (0-100)
    pub percent: u8,
    /// Estimated whole seconds remaining; None while the rate is unstable.
    pub eta_seconds: Option<u64>,
}

impl DownloadProgress {
    pub fn new(downloaded: u64, total: u64, speed_mbps: f64) -> Self {
        let percent = if total > 0 {
            ((downloaded as f64 / total as f64) * 100.0) as u8
        } else {
            0
        };
        Self {
            downloaded_bytes: downloaded,
            total_bytes: total,
            downloaded_mb: downloaded as f64 / (1024.0 * 1024.0),
            total_mb: total as f64 / (1024.0 * 1024.0),
            speed_mbps,
            percent,
            eta_seconds: None,
        }
    }

    pub fn with_eta_seconds(mut self, eta_seconds: Option<u64>) -> Self {
        self.eta_seconds = eta_seconds;
        self
    }
}

/// Model status in the system
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStatus {
    /// Model is not yet downloaded
    NotDownloaded,

    /// Model is currently being downloaded (progress 0-100)
    Downloading { progress: u8 },

    /// Model is downloaded and ready to use
    Available,

    /// Model file is corrupted and needs redownload
    Corrupted {
        file_size: u64,
        expected_min_size: u64,
    },

    /// Error occurred with the model
    Error(String),
}

/// Model information for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model name (e.g., "gemma3:1b")
    pub name: String,

    /// Display name for UI
    pub display_name: String,

    /// Current status
    pub status: ModelStatus,

    /// File path (if available)
    pub path: PathBuf,

    /// Size in MB
    pub size_mb: u64,

    /// Exact download size in bytes, straight from the catalogue.
    pub size_bytes: u64,

    /// Context window size in tokens
    pub context_size: u32,

    /// Description
    pub description: String,

    /// GGUF filename on disk
    pub gguf_file: String,
}

/// A second request for a model whose download is already running. Callers that
/// can treat a duplicate as harmless downcast to this instead of matching on the
/// message text, so the wording stays free to change.
#[derive(Debug, thiserror::Error)]
#[error("Download already in progress")]
pub(crate) struct DownloadAlreadyRunning;

pub(crate) fn is_download_already_running(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DownloadAlreadyRunning>().is_some()
}

/// Where a transfer's bytes live until it has finished and validated.
///
/// Publishing by rename is what keeps the size-band check at the top of
/// `download_reserved_model` honest - and `scan_models` with it, after a
/// restart. Both decide a file is a finished model by its size being within
/// ten percent of the catalogue figure, which a transfer truncated late is
/// too. Staging the bytes elsewhere means the model's real path only ever
/// holds a transfer some attempt completed, so there is nothing at that path
/// for the band to misread.
fn partial_download_path(file_path: &Path) -> PathBuf {
    let mut file_name = file_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(".download");

    file_path.with_file_name(file_name)
}

// ============================================================================
// Model Manager
// ============================================================================

pub struct ModelManager {
    /// Directory where models are stored
    models_dir: PathBuf,

    /// Currently available models with their status
    available_models: Arc<RwLock<HashMap<String, ModelInfo>>>,

    /// Active downloads (model names)
    active_downloads: Arc<RwLock<HashSet<String>>>,

    /// Cancellation flag for current download
    cancel_download_flag: Arc<RwLock<Option<String>>>,
}

impl ModelManager {
    /// Create a new model manager with default models directory
    pub fn new() -> Result<Self> {
        Self::new_with_models_dir(None)
    }

    /// Create a new model manager with custom models directory
    pub fn new_with_models_dir(models_dir: Option<PathBuf>) -> Result<Self> {
        let models_dir = if let Some(dir) = models_dir {
            dir
        } else {
            // Fallback: Use current directory in development
            let current_dir = std::env::current_dir()
                .map_err(|e| anyhow!("Failed to get current directory: {}", e))?;

            if cfg!(debug_assertions) {
                // Development mode
                current_dir.join("models").join("summary")
            } else {
                // Production mode fallback (caller should provide path)
                log::warn!("ModelManager: No models directory provided, using fallback path");
                dirs::data_dir()
                    .or_else(dirs::home_dir)
                    .ok_or_else(|| anyhow!("Could not find system data directory"))?
                    .join("Meetily")
                    .join("models")
                    .join("summary")
            }
        };

        log::info!(
            "Built-in AI ModelManager using directory: {}",
            models_dir.display()
        );

        Ok(Self {
            models_dir,
            available_models: Arc::new(RwLock::new(HashMap::new())),
            active_downloads: Arc::new(RwLock::new(HashSet::new())),
            cancel_download_flag: Arc::new(RwLock::new(None)),
        })
    }

    /// Initialize and scan for existing models
    pub async fn init(&self) -> Result<()> {
        // Create models directory if it doesn't exist
        if !self.models_dir.exists() {
            fs::create_dir_all(&self.models_dir).await?;
            log::info!("Created models directory: {}", self.models_dir.display());
        }

        // Scan for existing models
        self.scan_models().await?;

        Ok(())
    }

    /// Scan models directory and update status
    pub async fn scan_models(&self) -> Result<()> {
        let start = std::time::Instant::now();

        log::info!(
            "Starting model scan in directory: {}",
            self.models_dir.display()
        );

        let model_defs = get_available_models();
        let mut models_map = HashMap::new();

        for model_def in model_defs {
            let model_path = self.models_dir.join(&model_def.gguf_file);
            log::debug!(
                "Checking model '{}' at path: {}",
                model_def.name,
                model_path.display()
            );

            let is_actively_downloading = {
                let active = self.active_downloads.read().await;
                active.contains(&model_def.name)
            };

            // If actively downloading, preserve existing status from memory
            if is_actively_downloading {
                let existing_info = {
                    let models = self.available_models.read().await;
                    models.get(&model_def.name).cloned()
                };

                if let Some(info) = existing_info {
                    // Preserve existing status (should be Downloading)
                    models_map.insert(model_def.name.clone(), info);
                    log::debug!(
                        "Model '{}': Preserving Downloading status during scan",
                        model_def.name
                    );
                    continue;
                }
            }

            let status = if model_path.exists() {
                // Check if file size matches expected size (basic validation)
                match fs::metadata(&model_path).await {
                    Ok(metadata) => {
                        let file_size_mb = metadata.len() / (1024 * 1024);

                        // Allow 10% variance for file size check
                        let expected_min = (model_def.size_mb as f64 * 0.9) as u64;
                        let expected_max = (model_def.size_mb as f64 * 1.1) as u64;

                        log::info!(
                            "Model '{}': found {} MB (expected {}-{} MB)",
                            model_def.name,
                            file_size_mb,
                            expected_min,
                            expected_max
                        );

                        if file_size_mb >= expected_min && file_size_mb <= expected_max {
                            log::info!("Model '{}': AVAILABLE", model_def.name);
                            ModelStatus::Available
                        } else {
                            log::warn!(
                                "Model '{}': CORRUPTED (size mismatch: {} MB, expected {} MB)",
                                model_def.name,
                                file_size_mb,
                                model_def.size_mb
                            );
                            ModelStatus::Corrupted {
                                file_size: file_size_mb,
                                expected_min_size: expected_min,
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Model '{}': Failed to read metadata: {}", model_def.name, e);
                        ModelStatus::Error(format!("Failed to read metadata: {}", e))
                    }
                }
            } else {
                log::debug!("Model '{}': NOT FOUND", model_def.name);
                ModelStatus::NotDownloaded
            };

            let model_info = ModelInfo {
                name: model_def.name.clone(),
                display_name: model_def.display_name.clone(),
                status,
                path: model_path,
                size_mb: model_def.size_mb,
                size_bytes: model_def.size_bytes(),
                context_size: model_def.context_size,
                description: model_def.description.clone(),
                gguf_file: model_def.gguf_file.clone(),
            };

            models_map.insert(model_def.name.clone(), model_info);
        }

        let model_count = models_map.len();

        let mut models = self.available_models.write().await;
        *models = models_map;

        let elapsed = start.elapsed();
        log::info!(
            "Model scan complete: {} models checked in {:?}",
            model_count,
            elapsed
        );
        Ok(())
    }

    /// Get list of all models with their status
    pub async fn list_models(&self) -> Vec<ModelInfo> {
        self.available_models
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Get info for a specific model
    pub async fn get_model_info(&self, model_name: &str) -> Option<ModelInfo> {
        self.available_models.read().await.get(model_name).cloned()
    }

    /// Check if a model is ready to use
    /// If refresh=true, scans filesystem before checking (slower but accurate)
    pub async fn is_model_ready(&self, model_name: &str, refresh: bool) -> bool {
        if refresh {
            if let Err(e) = self.scan_models().await {
                log::error!("Failed to scan models: {}", e);
                return false;
            }
        }

        if let Some(info) = self.get_model_info(model_name).await {
            info.status == ModelStatus::Available
        } else {
            false
        }
    }

    /// Download a model with simple percentage callback (backward compatible)
    pub async fn download_model(
        &self,
        model_name: &str,
        progress_callback: Option<Box<dyn Fn(u8) + Send>>,
    ) -> Result<()> {
        // Wrap the simple callback to use detailed progress internally
        let detailed_callback: Option<Box<dyn Fn(DownloadProgress) + Send>> = progress_callback
            .map(|cb| {
                Box::new(move |p: DownloadProgress| cb(p.percent))
                    as Box<dyn Fn(DownloadProgress) + Send>
            });
        self.download_model_detailed(model_name, detailed_callback)
            .await
    }

    /// Download a model with detailed progress (MB, speed, etc.)
    pub async fn download_model_detailed(
        &self,
        model_name: &str,
        progress_callback: Option<Box<dyn Fn(DownloadProgress) + Send>>,
    ) -> Result<()> {
        log::info!("Starting download for model: {}", model_name);

        let model_def = get_model_by_name(model_name)
            .ok_or_else(|| anyhow!("Unknown model: {}", model_name))?;
        let file_path = self.models_dir.join(&model_def.gguf_file);

        self.download_model_detailed_from_source(
            model_name,
            &file_path,
            &model_def.download_url,
            model_def.size_mb,
            progress_callback,
        )
        .await
    }

    /// Reserve the model, run the transfer, and release the reservation once,
    /// whatever the transfer did.
    ///
    /// Taking the reservation before any network I/O is what makes a second
    /// request for the same model a duplicate rather than a second transfer
    /// competing for the same file. Releasing it from a single finaliser is what
    /// stops a connect that never got off the ground - no DNS, no route - from
    /// holding the name for the life of the process and rejecting every later
    /// attempt, including the user's own retry.
    async fn download_model_detailed_from_source(
        &self,
        model_name: &str,
        file_path: &Path,
        download_url: &str,
        expected_size_mb: u64,
        progress_callback: Option<Box<dyn Fn(DownloadProgress) + Send>>,
    ) -> Result<()> {
        // The only early return that skips the finaliser: the reservation this
        // collided with belongs to another attempt, so releasing it here would
        // pull the name out from under a transfer that is still running.
        self.reserve_active_download(model_name).await?;

        let result = self
            .download_reserved_model(
                model_name,
                file_path,
                download_url,
                expected_size_mb,
                progress_callback,
            )
            .await;
        self.finish_download(model_name, result).await
    }

    async fn reserve_active_download(&self, model_name: &str) -> Result<()> {
        let mut active = self.active_downloads.write().await;
        if !active.insert(model_name.to_string()) {
            log::warn!("Download already in progress for model: {}", model_name);
            return Err(DownloadAlreadyRunning.into());
        }

        Ok(())
    }

    /// Release the reservation `reserve_active_download` took, whatever the
    /// transfer did.
    async fn finish_download(&self, model_name: &str, result: Result<()>) -> Result<()> {
        self.active_downloads.write().await.remove(model_name);

        result
    }

    async fn download_reserved_model(
        &self,
        model_name: &str,
        file_path: &Path,
        download_url: &str,
        expected_size_mb: u64,
        progress_callback: Option<Box<dyn Fn(DownloadProgress) + Send>>,
    ) -> Result<()> {
        // Clear cancellation flag
        {
            let mut cancel_flag = self.cancel_download_flag.write().await;
            *cancel_flag = None;
        }

        // Update status to downloading
        {
            let mut models = self.available_models.write().await;
            if let Some(model_info) = models.get_mut(model_name) {
                model_info.status = ModelStatus::Downloading { progress: 0 };
            }
        }

        let partial_path = partial_download_path(file_path);

        // Check if model already exists and is valid (skip re-download)
        if file_path.exists() {
            if let Ok(metadata) = fs::metadata(file_path).await {
                let file_size_mb = metadata.len() / (1024 * 1024);
                let expected_min = (expected_size_mb as f64 * 0.9) as u64;
                let expected_max = (expected_size_mb as f64 * 1.1) as u64;

                if file_size_mb >= expected_min && file_size_mb <= expected_max {
                    log::info!(
                        "Model '{}' already exists and is valid ({} MB), skipping download",
                        model_name,
                        file_size_mb
                    );

                    // Update status to available
                    {
                        let mut models = self.available_models.write().await;
                        if let Some(model_info) = models.get_mut(model_name) {
                            model_info.status = ModelStatus::Available;
                        }
                    }

                    // Report 100% progress
                    if let Some(ref callback) = progress_callback {
                        let total = metadata.len();
                        callback(DownloadProgress::new(total, total, 0.0));
                    }

                    return Ok(());
                } else if file_size_mb > expected_max {
                    // File is LARGER than expected - possibly corrupted or wrong file
                    // Delete and re-download in this case
                    log::warn!(
                        "Model '{}' exists but is too large ({} MB, expected max {} MB), deleting and re-downloading",
                        model_name,
                        file_size_mb,
                        expected_max
                    );
                    if let Err(e) = fs::remove_file(file_path).await {
                        log::warn!("Failed to delete oversized model file: {}", e);
                    }
                } else {
                    // File is SMALLER than expected - a partial download. Don't
                    // delete it; move it to where the resume logic below looks.
                    // A partial only sits at the real path when an older version
                    // wrote it there, and leaving it would hand the size band
                    // something to misread once enough of it had arrived.
                    log::info!(
                        "Model '{}' exists but is incomplete ({} MB, expected min {} MB), will resume download",
                        model_name,
                        file_size_mb,
                        expected_min
                    );
                    if !partial_path.exists() {
                        if let Err(e) = fs::rename(file_path, &partial_path).await {
                            log::warn!(
                                "Failed to stage the existing partial download for resume: {}",
                                e
                            );
                        }
                    }
                }
            }
        }

        log::info!("Downloading from: {}", download_url);
        log::info!(
            "Staging to: {} (published to {} once complete)",
            partial_path.display(),
            file_path.display()
        );

        // Create models directory if needed
        if !self.models_dir.exists() {
            fs::create_dir_all(&self.models_dir).await?;
        }

        // Check for existing partial download to resume
        let existing_size: u64 = if partial_path.exists() {
            fs::metadata(&partial_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        // Download the file with optimized client settings
        let client = Client::builder()
            .tcp_nodelay(true) // Disable Nagle's algorithm for faster streaming
            .pool_max_idle_per_host(1) // Keep connection alive
            .timeout(Duration::from_secs(3600)) // 1 hour timeout for large files
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        // Build request with Range header if resuming
        let mut request = client.get(download_url);
        if existing_size > 0 {
            log::info!(
                "Resuming download from byte {} ({:.1} MB)",
                existing_size,
                existing_size as f64 / (1024.0 * 1024.0)
            );
            request = request.header("Range", format!("bytes={}-", existing_size));
        }

        let response = request
            .send()
            .await
            .map_err(|e| anyhow!("Failed to start download: {}", e))?;

        // Check response status - 200 OK (full download) or 206 Partial Content (resume)
        let (total_size, resuming) = if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            // Server supports resume - total size = existing + remaining
            let remaining = response.content_length().unwrap_or(0);
            log::info!(
                "Server supports resume, {} MB remaining",
                remaining / (1024 * 1024)
            );
            (existing_size + remaining, true)
        } else if response.status().is_success() {
            // Server doesn't support resume or fresh download
            if existing_size > 0 {
                log::warn!("Server doesn't support resume, starting fresh download");
            }
            (response.content_length().unwrap_or(0), false)
        } else {
            return Err(anyhow!(
                "Download failed with status: {}",
                response.status()
            ));
        };

        log::info!("Total size: {} MB", total_size / (1024 * 1024));

        // Open file for append if resuming, or create new
        let file = if resuming {
            OpenOptions::new()
                .write(true)
                .append(true)
                .open(&partial_path)
                .await
                .map_err(|e| anyhow!("Failed to open file for append: {}", e))?
        } else {
            fs::File::create(&partial_path)
                .await
                .map_err(|e| anyhow!("Failed to create file: {}", e))?
        };

        // Use 8MB buffer to reduce disk I/O syscalls (major performance improvement)
        let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);

        let mut downloaded: u64 = if resuming { existing_size } else { 0 };

        // Emit initial progress (showing resumed position if applicable)
        if let Some(ref callback) = progress_callback {
            callback(DownloadProgress::new(downloaded, total_size, 0.0));
        }
        log::info!(
            "Starting at {:.1} MB / {:.1} MB",
            downloaded as f64 / (1024.0 * 1024.0),
            total_size as f64 / (1024.0 * 1024.0)
        );

        let mut last_progress_percent = if total_size > 0 {
            ((downloaded as f64 / total_size as f64) * 100.0) as u8
        } else {
            0
        };
        let mut last_report_time = std::time::Instant::now();
        let mut bytes_since_last_report: u64 = 0;
        let download_start_time = std::time::Instant::now();
        let start_downloaded = downloaded;

        // Seed the estimator at the resumed position rather than letting the
        // first in-loop report be its baseline: otherwise the bytes already on
        // disk from a prior attempt would be counted as downloaded in whatever
        // sliver of time elapses before the next report, reading as a burst.
        let mut eta = DownloadEta::new();
        eta.observe(start_downloaded, download_start_time);

        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();

        loop {
            // Check for cancellation
            {
                let cancel_flag = self.cancel_download_flag.read().await;
                if cancel_flag.as_ref() == Some(&model_name.to_string()) {
                    log::info!("Download cancelled for model: {}", model_name);

                    // Flush and keep partial file for resume on next attempt
                    let _ = writer.flush().await;
                    drop(writer);

                    // Update status
                    {
                        let mut models = self.available_models.write().await;
                        if let Some(model_info) = models.get_mut(model_name) {
                            model_info.status = ModelStatus::NotDownloaded;
                        }
                    }

                    // Use special marker prefix to distinguish cancellation from other errors
                    return Err(anyhow!("CANCELLED: Download cancelled by user"));
                }
            }

            // Add per-chunk timeout (30 seconds) to detect stalled connections
            let next_result = timeout(Duration::from_secs(30), stream.next()).await;

            let chunk = match next_result {
                // Timeout - no data received for 30 seconds
                Err(_) => {
                    log::warn!(
                        "Download timeout for {}: no data received for 30 seconds",
                        model_name
                    );
                    let _ = writer.flush().await;

                    // Set model status to Error (NOT NotDownloaded) so UI can show retry button
                    {
                        let mut models = self.available_models.write().await;
                        if let Some(model_info) = models.get_mut(model_name) {
                            model_info.status = ModelStatus::Error(
                                "Download timeout - No data received for 30 seconds".to_string(),
                            );
                        }
                    }

                    return Err(anyhow!(
                        "Download timeout - No data received for 30 seconds"
                    ));
                }
                // Stream ended
                Ok(None) => break,
                // Got chunk result
                Ok(Some(chunk_result)) => {
                    match chunk_result {
                        Ok(c) => c,
                        // Detect error type for better user feedback
                        Err(e) => {
                            log::error!("Download error for {}: {:?}", model_name, e);
                            let _ = writer.flush().await;

                            // Categorize error for user-friendly message
                            let error_msg = if e.is_timeout() {
                                "Connection timeout - Check your internet"
                            } else if e.is_connect() {
                                "Connection failed - Check your internet"
                            } else if e.is_body() {
                                "Stream interrupted - Network unstable"
                            } else {
                                "Download error"
                            };

                            // Set model status to Error (NOT NotDownloaded) so UI can show retry button
                            {
                                let mut models = self.available_models.write().await;
                                if let Some(model_info) = models.get_mut(model_name) {
                                    model_info.status = ModelStatus::Error(error_msg.to_string());
                                }
                            }

                            return Err(anyhow!("{}: {}", error_msg, e));
                        }
                    }
                }
            };
            let chunk_len = chunk.len() as u64;
            writer
                .write_all(&chunk)
                .await
                .map_err(|e| anyhow!("Error writing to file: {}", e))?;

            downloaded += chunk_len;
            bytes_since_last_report += chunk_len;

            // Calculate progress
            let progress_percent = if total_size > 0 {
                let exact_percent = (downloaded as f64 / total_size as f64) * 100.0;
                exact_percent.min(100.0) as u8
            } else {
                0
            };

            let elapsed_since_report = last_report_time.elapsed();
            let is_download_complete = downloaded >= total_size;
            let should_report = progress_percent > last_progress_percent
                || is_download_complete  // Force report on completion
                || elapsed_since_report.as_millis() >= 500;

            if should_report {
                let report_instant = std::time::Instant::now();
                eta.observe(downloaded, report_instant);
                let eta_seconds = eta.seconds_remaining(downloaded, total_size);

                // Calculate speed based on bytes downloaded since last report
                let speed_mbps = if elapsed_since_report.as_secs_f64() > 0.0 {
                    (bytes_since_last_report as f64 / (1024.0 * 1024.0))
                        / elapsed_since_report.as_secs_f64()
                } else {
                    // Fallback to overall average speed
                    let total_elapsed = download_start_time.elapsed().as_secs_f64();
                    if total_elapsed > 0.0 {
                        ((downloaded - start_downloaded) as f64 / (1024.0 * 1024.0)) / total_elapsed
                    } else {
                        0.0
                    }
                };

                log::info!(
                    "Download: {:.1} MB / {:.1} MB ({:.1} MB/s)",
                    downloaded as f64 / (1024.0 * 1024.0),
                    total_size as f64 / (1024.0 * 1024.0),
                    speed_mbps
                );

                // Update status
                {
                    let mut models = self.available_models.write().await;
                    if let Some(model_info) = models.get_mut(model_name) {
                        model_info.status = ModelStatus::Downloading {
                            progress: if is_download_complete {
                                100
                            } else {
                                progress_percent
                            },
                        };
                    }
                }

                // Call progress callback with detailed info
                if let Some(ref callback) = progress_callback {
                    callback(
                        DownloadProgress::new(downloaded, total_size, speed_mbps)
                            .with_eta_seconds(eta_seconds),
                    );
                }

                last_progress_percent = progress_percent;
                last_report_time = report_instant;
                bytes_since_last_report = 0;
            }
        }

        writer.flush().await?;
        drop(writer);

        // The length the server declared is the only end-to-end check this path
        // has - there is no per-artifact manifest - and a stream that stops early
        // leaves a file whose first four bytes are still a valid GGUF magic
        // number, so validate_gguf_file would wave it through. The check belongs
        // here rather than next to that validation so a truncated transfer never
        // paints 100% on the user's screen and only then fails. A response with
        // no Content-Length makes total_size 0 and so lands here too: with no
        // declared size there is nothing to verify the transfer against, and
        // trusting it is what this check exists to stop.
        // The bytes stay on disk for the next attempt to resume from. They are
        // safe to keep because they are still at partial_path - the size band at
        // the top of this function never sees them, so failing here cannot be
        // undone by the retry it asks the user for.
        if downloaded != total_size {
            let error_msg =
                format!("Download stopped at {downloaded} bytes, expected {total_size} bytes");
            log::error!("Incomplete download for {}: {}", model_name, error_msg);

            {
                let mut models = self.available_models.write().await;
                if let Some(model_info) = models.get_mut(model_name) {
                    model_info.status = ModelStatus::Error(error_msg.clone());
                }
            }

            return Err(anyhow!(error_msg));
        }

        log::info!("Download completed for model: {}", model_name);

        {
            let mut models = self.available_models.write().await;
            if let Some(model_info) = models.get_mut(model_name) {
                model_info.status = ModelStatus::Downloading { progress: 100 };
            }
        }

        if let Some(ref callback) = progress_callback {
            callback(DownloadProgress::new(total_size, total_size, 0.0));
        }

        // Small delay to ensure UI receives 100% event
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        if let Err(e) = self.validate_gguf_file(&partial_path).await {
            log::error!("Downloaded file failed validation: {}", e);

            // Clean up invalid file
            let _ = fs::remove_file(&partial_path).await;

            // Update status
            {
                let mut models = self.available_models.write().await;
                if let Some(model_info) = models.get_mut(model_name) {
                    model_info.status = ModelStatus::Error(format!("Validation failed: {}", e));
                }
            }

            return Err(anyhow!("File validation failed: {}", e));
        }

        // Publish. Everything above ran against partial_path, so this rename is
        // the single moment the model becomes readable to the rest of the app -
        // and the reason a file at file_path can be trusted to be whole.
        if let Err(e) = fs::rename(&partial_path, file_path).await {
            log::error!("Failed to publish the downloaded model: {}", e);

            {
                let mut models = self.available_models.write().await;
                if let Some(model_info) = models.get_mut(model_name) {
                    model_info.status =
                        ModelStatus::Error(format!("Failed to store the download: {}", e));
                }
            }

            return Err(anyhow!("Failed to store the downloaded model: {}", e));
        }

        // Update status to available
        {
            let mut models = self.available_models.write().await;
            if let Some(model_info) = models.get_mut(model_name) {
                model_info.status = ModelStatus::Available;
                model_info.path = file_path.to_path_buf();
            }
        }

        Ok(())
    }

    /// Validate that a file is a valid GGUF model
    async fn validate_gguf_file(&self, path: &Path) -> Result<()> {
        let mut file = fs::File::open(path).await?;

        // Read first 4 bytes to check for GGUF magic number
        use tokio::io::AsyncReadExt;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).await?;

        // GGUF magic number is "GGUF" (0x47475546)
        if &magic == b"GGUF" {
            Ok(())
        } else if &magic == b"ggjt" || &magic == b"ggla" || &magic == b"ggml" {
            // Older formats (GGML, GGJT)
            Ok(())
        } else {
            Err(anyhow!(
                "Invalid model file: magic number {:?} doesn't match GGUF/GGML",
                magic
            ))
        }
    }

    /// Cancel an ongoing download
    pub async fn cancel_download(&self, model_name: &str) -> Result<()> {
        log::info!("Cancelling download for model: {}", model_name);

        // Set cancellation flag - the download loop will detect this and stop
        {
            let mut cancel_flag = self.cancel_download_flag.write().await;
            *cancel_flag = Some(model_name.to_string());
        }

        // Note: the reservation is released by finish_download once the loop has
        // unwound, not here, so cancelling cannot free a name the transfer is
        // still using.

        // Update status immediately for UI responsiveness
        {
            let mut models = self.available_models.write().await;
            if let Some(model_info) = models.get_mut(model_name) {
                model_info.status = ModelStatus::NotDownloaded;
            }
        }

        // Brief delay to let download loop detect cancellation
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Ok(())
    }

    /// Delete a corrupted or available model file
    pub async fn delete_model(&self, model_name: &str) -> Result<()> {
        log::info!("Deleting model: {}", model_name);

        let model_def = get_model_by_name(model_name)
            .ok_or_else(|| anyhow!("Unknown model: {}", model_name))?;

        let file_path = self.models_dir.join(&model_def.gguf_file);

        if file_path.exists() {
            fs::remove_file(&file_path).await?;
            log::info!("Deleted model file: {}", file_path.display());
        }

        // An abandoned transfer's bytes are invisible to every status check, so
        // nothing else would ever clear them.
        let partial_path = partial_download_path(&file_path);
        if partial_path.exists() {
            if let Err(e) = fs::remove_file(&partial_path).await {
                log::warn!("Failed to delete partial download file: {}", e);
            } else {
                log::info!("Deleted partial download: {}", partial_path.display());
            }
        }

        // Update status
        {
            let mut models = self.available_models.write().await;
            if let Some(model_info) = models.get_mut(model_name) {
                model_info.status = ModelStatus::NotDownloaded;
            }
        }

        Ok(())
    }

    /// Get models directory path
    pub fn get_models_directory(&self) -> PathBuf {
        self.models_dir.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    const TEST_MODEL_NAME: &str = "gemma3:1b";
    /// Large enough that the few bytes these tests transfer never look like a
    /// complete file to the "already exists and is valid" shortcut.
    const TEST_MODEL_SIZE_MB: u64 = 8;

    struct TestResponse {
        status: &'static str,
        content_length: Option<u64>,
        /// The Range header this request must carry, or None for no Range at
        /// all. Asserted, because "did the retry resume or start over?" is the
        /// question several of these tests exist to answer.
        expected_range: Option<&'static str>,
        head: &'static [u8],
        rest: &'static [u8],
        /// Holds the response open after `head`, so a test can act on a download
        /// that is genuinely in flight.
        release_before_rest: Option<oneshot::Receiver<()>>,
    }

    impl TestResponse {
        fn new(status: &'static str, content_length: Option<u64>, body: &'static [u8]) -> Self {
            Self {
                status,
                content_length,
                expected_range: None,
                head: body,
                rest: b"",
                release_before_rest: None,
            }
        }

        fn resuming_from(mut self, range: &'static str) -> Self {
            self.expected_range = Some(range);
            self
        }
    }

    fn complete_response(body: &'static [u8]) -> TestResponse {
        TestResponse::new("200 OK", Some(body.len() as u64), body)
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let read = socket.read(&mut buffer).await.expect("read request");
            assert_ne!(read, 0, "request ended before its headers");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8(request).expect("request is valid UTF-8");
            }
        }
    }

    async fn serve_one_download(expected: TestResponse) -> (String, tokio::task::JoinHandle<()>) {
        serve_downloads(vec![expected]).await
    }

    /// Answer one request per entry, in order. The client sends
    /// `Connection: close`, so each attempt arrives on its own connection.
    async fn serve_downloads(
        expected_requests: Vec<TestResponse>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback test server");
        let address = listener.local_addr().expect("read loopback address");
        let server = tokio::spawn(async move {
            for expected in expected_requests {
                let (mut socket, _) = listener.accept().await.expect("accept test request");
                let request = read_request(&mut socket).await;
                assert!(
                    request.starts_with("GET /model.gguf HTTP/"),
                    "unexpected request path: {request}"
                );
                match expected.expected_range {
                    Some(range) => assert!(
                        request.contains(&format!("range: bytes={range}"))
                            || request.contains(&format!("Range: bytes={range}")),
                        "expected a request resuming from {range}, got: {request}"
                    ),
                    None => assert!(
                        !request.to_lowercase().contains("range:"),
                        "expected a request with no Range header, got: {request}"
                    ),
                }

                let mut headers = format!("HTTP/1.1 {}\r\nConnection: close\r\n", expected.status);
                if let Some(content_length) = expected.content_length {
                    headers.push_str(&format!("Content-Length: {content_length}\r\n"));
                }
                headers.push_str("\r\n");
                socket
                    .write_all(headers.as_bytes())
                    .await
                    .expect("write response headers");
                socket
                    .write_all(expected.head)
                    .await
                    .expect("write start of response body");
                socket.flush().await.expect("flush start of response body");
                if let Some(release_before_rest) = expected.release_before_rest {
                    release_before_rest
                        .await
                        .expect("release the held response");
                }
                socket
                    .write_all(expected.rest)
                    .await
                    .expect("write rest of response body");
            }
        });

        (format!("http://{address}/model.gguf"), server)
    }

    /// Join with a deadline. A regression that stops the client making a request
    /// the server is still waiting for - the retry taking a shortcut instead of
    /// going to the network, say - then fails the test instead of hanging it.
    async fn join_server(server: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the test server received every request it was set up to answer")
            .expect("join test server");
    }

    /// A loopback address with nothing listening on it, so connecting fails the
    /// way it does with no network at all.
    async fn unreachable_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback address");
        let address = listener.local_addr().expect("read loopback address");
        drop(listener);

        format!("http://{address}/model.gguf")
    }

    async fn test_manager() -> (tempfile::TempDir, Arc<ModelManager>, PathBuf) {
        let temp_dir = tempdir().expect("create temporary models directory");
        let manager = Arc::new(
            ModelManager::new_with_models_dir(Some(temp_dir.path().to_path_buf()))
                .expect("create model manager"),
        );
        manager.init().await.expect("initialise model manager");
        let file_path = temp_dir.path().join("test-model.gguf");

        (temp_dir, manager, file_path)
    }

    async fn is_reserved(manager: &ModelManager) -> bool {
        manager
            .active_downloads
            .read()
            .await
            .contains(TEST_MODEL_NAME)
    }

    async fn test_model_status(manager: &ModelManager) -> ModelStatus {
        manager
            .available_models
            .read()
            .await
            .get(TEST_MODEL_NAME)
            .expect("test model stays registered")
            .status
            .clone()
    }

    #[tokio::test]
    async fn failed_connection_releases_the_reservation_for_the_next_attempt() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        let url = unreachable_url().await;

        let error = manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                None,
            )
            .await
            .expect_err("connecting to a dead address must fail");
        assert!(error.to_string().contains("Failed to start download"));
        assert!(!is_reserved(&manager).await);

        let retry = manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                None,
            )
            .await
            .expect_err("the retry reaches the network and fails there too");
        assert!(!is_download_already_running(&retry));
        assert!(retry.to_string().contains("Failed to start download"));
        assert!(!is_reserved(&manager).await);
    }

    #[tokio::test]
    async fn duplicate_request_is_rejected_without_disturbing_the_running_download() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        let (release_tx, release_rx) = oneshot::channel();
        let (first_chunk_tx, first_chunk_rx) = oneshot::channel();
        let first_chunk_tx = Arc::new(Mutex::new(Some(first_chunk_tx)));
        let (url, server) = serve_one_download(TestResponse {
            rest: b"TAIL",
            release_before_rest: Some(release_rx),
            ..TestResponse::new("200 OK", Some(8), b"GGUF")
        })
        .await;

        let download_manager = Arc::clone(&manager);
        let download_path = file_path.clone();
        let download_url = url.clone();
        let download = tokio::spawn(async move {
            download_manager
                .download_model_detailed_from_source(
                    TEST_MODEL_NAME,
                    &download_path,
                    &download_url,
                    TEST_MODEL_SIZE_MB,
                    Some(Box::new(move |progress| {
                        if progress.downloaded_bytes >= 4 {
                            if let Some(sender) =
                                first_chunk_tx.lock().expect("lock chunk sender").take()
                            {
                                let _ = sender.send(());
                            }
                        }
                    })),
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), first_chunk_rx)
            .await
            .expect("observe the first chunk of the held download")
            .expect("first chunk sender stays connected");

        let duplicate = manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                None,
            )
            .await
            .expect_err("a second request for a running download must be rejected");
        assert!(is_download_already_running(&duplicate));
        assert!(is_reserved(&manager).await);

        release_tx.send(()).expect("release the held response");
        download
            .await
            .expect("join download task")
            .expect("the first download still completes");
        join_server(server).await;

        assert!(!is_reserved(&manager).await);
        assert_eq!(fs::read(&file_path).await.unwrap(), b"GGUFTAIL");
        assert_eq!(test_model_status(&manager).await, ModelStatus::Available);
    }

    #[tokio::test]
    async fn transfer_without_a_declared_length_fails_instead_of_completing() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        let (url, server) = serve_one_download(TestResponse::new("200 OK", None, b"GGUF")).await;

        let error = manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                None,
            )
            .await
            .expect_err("a transfer with nothing to verify it against must not be trusted");
        join_server(server).await;

        assert!(error
            .to_string()
            .contains("Download stopped at 4 bytes, expected 0 bytes"));
        assert!(matches!(
            test_model_status(&manager).await,
            ModelStatus::Error(_)
        ));
        // The bytes stay on disk for the next attempt to resume from - staged,
        // not published, so nothing can mistake them for a finished model.
        assert!(!file_path.exists());
        assert_eq!(
            fs::read(partial_download_path(&file_path)).await.unwrap(),
            b"GGUF"
        );
        assert!(!is_reserved(&manager).await);
    }

    #[tokio::test]
    async fn short_response_fails_before_gguf_validation_can_accept_it() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let callback_events = Arc::clone(&events);
        let (url, server) = serve_one_download(TestResponse::new("200 OK", Some(8), b"GGUF")).await;

        let error = manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                Some(Box::new(move |progress| {
                    callback_events
                        .lock()
                        .expect("lock progress events")
                        .push(progress);
                })),
            )
            .await
            .expect_err("half a file must not pass as a download");
        join_server(server).await;

        // The four bytes that did arrive are a valid GGUF magic number, so the
        // failure has to come from the byte count rather than from validation.
        assert!(!error.to_string().contains("validation"));
        assert!(events
            .lock()
            .expect("lock progress events")
            .iter()
            .all(|progress| progress.percent < 100));
        assert!(matches!(
            test_model_status(&manager).await,
            ModelStatus::Error(_)
        ));
        assert!(!file_path.exists());
        assert_eq!(
            fs::read(partial_download_path(&file_path)).await.unwrap(),
            b"GGUF"
        );
        assert!(!is_reserved(&manager).await);
    }

    /// The size band accepts anything within ten percent of the catalogue
    /// figure, and integer-truncated megabytes make a 1 MiB model's band
    /// [0, 1] MB - so four bytes stand in here for the 1150 MB of a 1221 MB
    /// model that a proxy cut short. Same branch, same arithmetic, no
    /// multi-megabyte fixture.
    const BAND_ACCEPTS_ANY_SMALL_FILE_MB: u64 = 1;

    #[tokio::test]
    async fn truncated_transfer_is_not_mistaken_for_a_finished_model_on_the_next_attempt() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        let (url, server) = serve_downloads(vec![
            TestResponse::new("200 OK", Some(8), b"GGUF"),
            TestResponse::new("206 Partial Content", Some(4), b"TAIL").resuming_from("4-"),
        ])
        .await;

        manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                BAND_ACCEPTS_ANY_SMALL_FILE_MB,
                None,
            )
            .await
            .expect_err("a transfer cut short must fail");

        // Nothing was published, so the size band at the top of the next attempt
        // has nothing to accept - which is what stops the retry below from
        // undoing the failure above.
        assert!(!file_path.exists());
        assert_eq!(
            fs::read(partial_download_path(&file_path)).await.unwrap(),
            b"GGUF"
        );

        manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                BAND_ACCEPTS_ANY_SMALL_FILE_MB,
                None,
            )
            .await
            .expect("the retry resumes the staged bytes and finishes the file");

        assert_eq!(fs::read(&file_path).await.unwrap(), b"GGUFTAIL");
        join_server(server).await;
        assert!(!partial_download_path(&file_path).exists());
        assert_eq!(test_model_status(&manager).await, ModelStatus::Available);
        assert!(!is_reserved(&manager).await);
    }

    #[tokio::test]
    async fn partial_left_at_the_published_path_by_an_older_version_is_staged_and_resumed() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        fs::write(&file_path, b"GGUF")
            .await
            .expect("seed a partial written straight to the published path");
        let (url, server) = serve_one_download(
            TestResponse::new("206 Partial Content", Some(4), b"TAIL").resuming_from("4-"),
        )
        .await;

        manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                None,
            )
            .await
            .expect("the staged partial is resumed rather than re-fetched");
        join_server(server).await;

        assert_eq!(fs::read(&file_path).await.unwrap(), b"GGUFTAIL");
        assert!(!partial_download_path(&file_path).exists());
        assert_eq!(test_model_status(&manager).await, ModelStatus::Available);
    }

    #[tokio::test]
    async fn completed_download_releases_the_reservation_for_a_later_one() {
        let (_temp_dir, manager, file_path) = test_manager().await;
        let (url, server) = serve_one_download(complete_response(b"GGUF")).await;

        manager
            .download_model_detailed_from_source(
                TEST_MODEL_NAME,
                &file_path,
                &url,
                TEST_MODEL_SIZE_MB,
                None,
            )
            .await
            .expect("a complete transfer succeeds");
        join_server(server).await;

        assert_eq!(test_model_status(&manager).await, ModelStatus::Available);
        assert!(!is_reserved(&manager).await);
        assert!(manager
            .reserve_active_download(TEST_MODEL_NAME)
            .await
            .is_ok());
    }
}
