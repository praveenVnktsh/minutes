//! The Nemotron Speech Streaming 0.6B live model: where it lives, whether it
//! is downloaded, and how to download it.
//!
//! Source: the sherpa-onnx export by the sherpa-onnx author (int8, 560 ms
//! chunks) of `nvidia/nemotron-speech-streaming-en-0.6b`, pinned to one
//! revision so every file always matches its size and SHA-256 below.
//!
//! Not to be confused with the Nemotron-3 *diarization* model (Sortformer) in
//! `audio/sortformer`, which is a different network for speaker labels.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Identifier shown in settings and recorded as model provenance.
pub const MODEL_ID: &str = "nemotron-speech-streaming-en-0.6b-560ms-int8";
/// Folder under `app_data_dir/models`.
const MODEL_DIR: &str = "nemotron-streaming";
const REPO: &str =
    "csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25";
/// Pinned Hugging Face revision of [`REPO`].
pub const REVISION: &str = "52056fdc070914a48dcd68b31b44d6a6f5b85902";
/// Event carrying download progress to the frontend.
pub const DOWNLOAD_PROGRESS_EVENT: &str = "live-model-download-progress";

pub struct ModelFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

pub const ENCODER: &str = "encoder.int8.onnx";
pub const DECODER: &str = "decoder.int8.onnx";
pub const JOINER: &str = "joiner.int8.onnx";
pub const TOKENS: &str = "tokens.txt";

/// Every file the recognizer needs, at [`REVISION`].
pub const FILES: [ModelFile; 4] = [
    ModelFile {
        name: TOKENS,
        bytes: 8_952,
        sha256: "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff",
    },
    ModelFile {
        name: DECODER,
        bytes: 7_257_753,
        sha256: "0be9702c2f427a2b6bb241d298e0d3836a558de1f5b9fd3018f1cce6e2b3fa98",
    },
    ModelFile {
        name: JOINER,
        bytes: 1_735_862,
        sha256: "a35eac38a22ebceb04d230ed7afe0d68f446ba6914a036b97f14fece95967e23",
    },
    ModelFile {
        name: ENCODER,
        bytes: 652_916_849,
        sha256: "7d932213491ad355c6e5576705dc3494731a52af87d7a1b954559340147909d8",
    },
];

pub fn total_bytes() -> u64 {
    FILES.iter().map(|file| file.bytes).sum()
}

pub fn file_url(name: &str) -> String {
    format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{name}")
}

pub fn model_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("models").join(MODEL_DIR))
}

/// A file only reaches its final name after its hash was verified, so a file
/// of the expected size is trusted without re-hashing 650 MB on every start.
pub fn is_downloaded_in(dir: &Path) -> bool {
    FILES.iter().all(|file| {
        std::fs::metadata(dir.join(file.name))
            .map(|metadata| metadata.len() == file.bytes)
            .unwrap_or(false)
    })
}

pub fn is_downloaded<R: Runtime>(app: &AppHandle<R>) -> bool {
    model_dir(app)
        .map(|dir| is_downloaded_in(&dir))
        .unwrap_or(false)
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static DOWNLOAD_TASK: Mutex<Option<tauri::async_runtime::JoinHandle<()>>> = Mutex::new(None);

pub fn is_downloading() -> bool {
    DOWNLOADING.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgressEvent {
    #[serde(rename = "modelName")]
    pub model_name: &'static str,
    pub progress: u32,
    pub downloaded_mb: f64,
    pub total_mb: f64,
    pub speed_mbps: f64,
    pub eta_seconds: Option<u64>,
    /// downloading | completed | error | cancelled
    pub status: &'static str,
    pub error: Option<String>,
}

const MB: f64 = 1024.0 * 1024.0;

fn emit<R: Runtime>(app: &AppHandle<R>, event: DownloadProgressEvent) {
    if let Err(error) = app.emit(DOWNLOAD_PROGRESS_EVENT, event) {
        warn!("Failed to emit live model download progress: {error}");
    }
}

fn terminal_event(status: &'static str, error: Option<String>) -> DownloadProgressEvent {
    let total = total_bytes() as f64 / MB;
    DownloadProgressEvent {
        model_name: MODEL_ID,
        progress: if status == "completed" { 100 } else { 0 },
        downloaded_mb: if status == "completed" { total } else { 0.0 },
        total_mb: total,
        speed_mbps: 0.0,
        eta_seconds: None,
        status,
        error,
    }
}

/// Overall progress after `done_bytes` of finished files plus `percent` of
/// the file in flight.
pub fn overall_bytes(done_bytes: u64, file_bytes: u64, percent: u32) -> u64 {
    done_bytes + file_bytes * u64::from(percent.min(100)) / 100
}

async fn download_all<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let dir = model_dir(app)?;
    tokio::fs::create_dir_all(&dir).await?;
    let client = reqwest::Client::builder()
        .connect_timeout(crate::audio::diarization::DOWNLOAD_CONNECT_TIMEOUT)
        .build()?;
    let total = total_bytes();
    let mut done: u64 = 0;
    let mut eta = crate::download_eta::DownloadEta::new();

    for file in FILES.iter() {
        let destination = dir.join(file.name);
        if std::fs::metadata(&destination)
            .map(|m| m.len() == file.bytes)
            .unwrap_or(false)
        {
            done += file.bytes;
            continue;
        }
        let _ = tokio::fs::remove_file(&destination).await;
        info!(
            "Live model: downloading {} ({} bytes)",
            file.name, file.bytes
        );
        let done_before = done;
        crate::audio::diarization::download_verified(
            &client,
            &file_url(file.name),
            &destination,
            file.bytes,
            file.sha256,
            |percent| {
                let downloaded = overall_bytes(done_before, file.bytes, percent);
                eta.observe(downloaded, Instant::now());
                emit(
                    app,
                    DownloadProgressEvent {
                        model_name: MODEL_ID,
                        progress: (downloaded * 100 / total.max(1)) as u32,
                        downloaded_mb: downloaded as f64 / MB,
                        total_mb: total as f64 / MB,
                        speed_mbps: eta.bytes_per_second().unwrap_or(0.0) / MB,
                        eta_seconds: eta.seconds_remaining(downloaded, total),
                        status: "downloading",
                        error: None,
                    },
                );
            },
        )
        .await
        .with_context(|| format!("Failed to download the live model file {}", file.name))?;
        done += file.bytes;
    }
    Ok(())
}

/// Download the model and wait for it. Emits progress; a second caller while a
/// download runs gets an error instead of a duplicate download.
pub async fn download<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    if is_downloaded(app) {
        emit(app, terminal_event("completed", None));
        return Ok(());
    }
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err(anyhow!("The live model is already downloading"));
    }
    let task_app = app.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = tauri::async_runtime::spawn(async move {
        let result = download_all(&task_app).await;
        let _ = sender.send(result);
    });
    *DOWNLOAD_TASK.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);

    let result = match receiver.await {
        Ok(result) => result,
        // The task was aborted by `cancel_download`.
        Err(_) => Err(anyhow!("cancelled")),
    };
    DOWNLOADING.store(false, Ordering::SeqCst);
    DOWNLOAD_TASK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();

    match &result {
        Ok(()) => {
            info!("Live model downloaded and verified");
            emit(app, terminal_event("completed", None));
        }
        Err(error) if error.to_string() == "cancelled" => {
            remove_partials(app);
            emit(app, terminal_event("cancelled", None));
        }
        Err(error) => {
            warn!("Live model download failed: {error:#}");
            emit(app, terminal_event("error", Some(format!("{error:#}"))));
        }
    }
    result
}

fn remove_partials<R: Runtime>(app: &AppHandle<R>) {
    if let Ok(dir) = model_dir(app) {
        for file in FILES.iter() {
            let _ = std::fs::remove_file(dir.join(file.name).with_extension("part"));
        }
    }
}

/// Abort a running download. Returns whether one was running.
pub fn cancel_download() -> bool {
    match DOWNLOAD_TASK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        Some(handle) => {
            handle.abort();
            true
        }
        None => false,
    }
}

/// Delete the downloaded model.
pub fn delete<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    super::engine::unload();
    let dir = model_dir(app)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_pinned_to_the_revision() {
        let url = file_url(ENCODER);
        assert!(url.contains(REVISION), "{url}");
        assert!(url.ends_with("/encoder.int8.onnx"));
        assert!(!url.contains("/main/"));
    }

    #[test]
    fn every_file_has_a_sha256_and_size() {
        for file in FILES.iter() {
            assert_eq!(file.sha256.len(), 64, "{}", file.name);
            assert!(file.bytes > 0);
        }
        assert!(total_bytes() > 600 * 1024 * 1024);
    }

    #[test]
    fn readiness_requires_every_file_at_its_exact_size() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_downloaded_in(dir.path()));
        for file in FILES.iter() {
            let f = std::fs::File::create(dir.path().join(file.name)).unwrap();
            f.set_len(file.bytes).unwrap();
        }
        assert!(is_downloaded_in(dir.path()));
        std::fs::File::create(dir.path().join(ENCODER))
            .unwrap()
            .set_len(10)
            .unwrap();
        assert!(!is_downloaded_in(dir.path()));
    }

    #[test]
    fn overall_progress_counts_finished_files_and_the_one_in_flight() {
        assert_eq!(overall_bytes(0, 200, 50), 100);
        assert_eq!(overall_bytes(1_000, 200, 100), 1_200);
        assert_eq!(overall_bytes(1_000, 200, 150), 1_200);
    }
}
