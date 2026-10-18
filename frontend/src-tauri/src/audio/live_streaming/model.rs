//! The Nemotron Speech Streaming 0.6B live model: where it lives, whether it
//! is downloaded, and how to download it.
//!
//! It comes in two bundles of the same network
//! (`nvidia/nemotron-speech-streaming-en-0.6b`), each pinned to one revision so
//! every file always matches its size and SHA-256 below:
//! - [`SHERPA`]: the sherpa-onnx export by the sherpa-onnx author (int8,
//!   560 ms chunks), run on the CPU;
//! - [`COREML`]: the Core ML export by FluidInference (int8 encoder, the same
//!   560 ms chunks), run on the GPU or Neural Engine by `nemotron-helper`.
//!
//! "The live model" in the UI is the bundle the engine setting needs
//! ([`needed_bundle`]); the other one is only used as a fallback when it is
//! already on disk.
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

use crate::audio::transcription_settings::LiveEngineChoice;

/// Event carrying download progress to the frontend.
pub const DOWNLOAD_PROGRESS_EVENT: &str = "live-model-download-progress";

pub struct ModelFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// One downloadable set of model files.
pub struct Bundle {
    /// Identifier shown in settings and recorded as model provenance.
    pub id: &'static str,
    /// Folder under `app_data_dir/models`.
    pub dir: &'static str,
    pub repo: &'static str,
    /// Pinned Hugging Face revision of `repo`.
    pub revision: &'static str,
    /// Folder of the repo the files are in, if not its root.
    pub subdir: Option<&'static str>,
    pub files: &'static [ModelFile],
}

pub const ENCODER: &str = "encoder.int8.onnx";
pub const DECODER: &str = "decoder.int8.onnx";
pub const JOINER: &str = "joiner.int8.onnx";
pub const TOKENS: &str = "tokens.txt";

pub const SHERPA: Bundle = Bundle {
    id: "nemotron-speech-streaming-en-0.6b-560ms-int8",
    dir: "nemotron-streaming",
    repo: "csukuangfj2/sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25",
    revision: "52056fdc070914a48dcd68b31b44d6a6f5b85902",
    subdir: None,
    files: &[
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
    ],
};

pub const COREML: Bundle = Bundle {
    id: "nemotron-speech-streaming-en-0.6b-560ms-coreml-int8",
    dir: "nemotron-streaming-coreml",
    repo: "FluidInference/nemotron-speech-streaming-en-0.6b-coreml",
    revision: "e673531caa6d25ab7baf5a8c14c9b99ba1551838",
    // The repo holds several chunk sizes; the app uses the 560 ms one, the
    // same chunking as `SHERPA`. File names are relative to that folder.
    subdir: Some("nemotron_coreml_560ms"),
    files: &[
        ModelFile {
            name: "metadata.json",
            bytes: 438,
            sha256: "e8d93e5afa1e5c87257f6e31a5c336058eeba7d6cacd36ba80fbf9758c3916dc",
        },
        ModelFile {
            name: "tokenizer.json",
            bytes: 18_374,
            sha256: "a87b5b8a0e0a989fa7db3121f279ce6af70e337feb55ab553b8d0658db70b9d3",
        },
        ModelFile {
            name: "decoder.mlmodelc/analytics/coremldata.bin",
            bytes: 243,
            sha256: "bca0e343fa0d698de8bdb22c66fd377b79104f606b1bfffc90931fe2161029b8",
        },
        ModelFile {
            name: "decoder.mlmodelc/coremldata.bin",
            bytes: 493,
            sha256: "4d0549ac72a38abdcbe7927f2b7d488386d6b96f1f5e837d93ba9e379de04f4e",
        },
        ModelFile {
            name: "decoder.mlmodelc/model.mil",
            bytes: 13_093,
            sha256: "8298f16d3926565b77732b549fa9c64fedebb2e5a29a76e33d0b2e970e93ae69",
        },
        ModelFile {
            name: "decoder.mlmodelc/weights/weight.bin",
            bytes: 14_429_952,
            sha256: "d9758e49f9b2b9a0462c933256663d4174209739e62f0ddc14e4147bc50e7818",
        },
        ModelFile {
            name: "joint.mlmodelc/analytics/coremldata.bin",
            bytes: 243,
            sha256: "478804308534c7f9cab1030aa9ce19ddf672fcd15deba633fe3ed5da8f53b103",
        },
        ModelFile {
            name: "joint.mlmodelc/coremldata.bin",
            bytes: 401,
            sha256: "46363626fd89730ade6e6461e8be808c7fade198fb0df63db83fb00ed033abb2",
        },
        ModelFile {
            name: "joint.mlmodelc/model.mil",
            bytes: 5_064,
            sha256: "73bcc51b58ff73c7620e31a2c32dc5ef4939e8ec2a77ec22242303422bfafba4",
        },
        ModelFile {
            name: "joint.mlmodelc/weights/weight.bin",
            bytes: 3_446_978,
            sha256: "d1d45514a91da39f24e264712a9f8b5a2a90e9cbce04bad3f3f74242bea106d5",
        },
        ModelFile {
            name: "encoder/encoder_int8.mlmodelc/analytics/coremldata.bin",
            bytes: 243,
            sha256: "ef51dbab5c6ce310ce1a537538a4df6b88f13ad5c45c0acaddca055503896daa",
        },
        ModelFile {
            name: "encoder/encoder_int8.mlmodelc/coremldata.bin",
            bytes: 608,
            sha256: "864a357ae2eb7d6b38ced73d5f23c908096a256516afa498c6730778767ec404",
        },
        ModelFile {
            name: "encoder/encoder_int8.mlmodelc/model.mil",
            bytes: 968_920,
            sha256: "4b903ee4f401d81add07d1252192a486bd07154174818902f03bca38b2f293f6",
        },
        ModelFile {
            name: "encoder/encoder_int8.mlmodelc/weights/weight.bin",
            bytes: 589_057_600,
            sha256: "8ced31411f7903eaec52de85e94c7847228f3e44028d8f0040800e5603943a40",
        },
    ],
};

impl Bundle {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    pub fn file_url(&self, name: &str) -> String {
        let path = match self.subdir {
            Some(subdir) => format!("{subdir}/{name}"),
            None => name.to_string(),
        };
        format!(
            "https://huggingface.co/{}/resolve/{}/{path}",
            self.repo, self.revision
        )
    }

    /// A file only reaches its final name after its hash was verified, so a
    /// file of the expected size is trusted without re-hashing on every start.
    pub fn is_downloaded_in(&self, dir: &Path) -> bool {
        self.files.iter().all(|file| {
            std::fs::metadata(dir.join(file.name))
                .map(|metadata| metadata.len() == file.bytes)
                .unwrap_or(false)
        })
    }

    pub fn dir<R: Runtime>(&self, app: &AppHandle<R>) -> Result<PathBuf> {
        Ok(app.path().app_data_dir()?.join("models").join(self.dir))
    }

    pub fn is_downloaded<R: Runtime>(&self, app: &AppHandle<R>) -> bool {
        self.dir(app)
            .map(|dir| self.is_downloaded_in(&dir))
            .unwrap_or(false)
    }
}

/// The bundle an engine choice runs: the sherpa export for the CPU, the Core
/// ML one otherwise.
pub fn bundle_for(choice: LiveEngineChoice) -> &'static Bundle {
    match choice {
        LiveEngineChoice::Cpu => &SHERPA,
        _ => &COREML,
    }
}

pub fn needed_bundle<R: Runtime>(app: &AppHandle<R>) -> &'static Bundle {
    bundle_for(crate::audio::transcription_settings::load_settings(app).live_engine)
}

/// Whether the live model for the current engine setting is on disk.
pub fn is_downloaded<R: Runtime>(app: &AppHandle<R>) -> bool {
    needed_bundle(app).is_downloaded(app)
}

/// Whether some live engine can run: the needed bundle, or the other one as a
/// fallback while the needed one is still downloading.
pub fn is_usable<R: Runtime>(app: &AppHandle<R>) -> bool {
    SHERPA.is_downloaded(app)
        || (COREML.is_downloaded(app) && super::native::resolve_helper().is_some())
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

fn terminal_event(
    bundle: &'static Bundle,
    status: &'static str,
    error: Option<String>,
) -> DownloadProgressEvent {
    let total = bundle.total_bytes() as f64 / MB;
    DownloadProgressEvent {
        model_name: bundle.id,
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

async fn download_all<R: Runtime>(app: &AppHandle<R>, bundle: &'static Bundle) -> Result<()> {
    let dir = bundle.dir(app)?;
    tokio::fs::create_dir_all(&dir).await?;
    let client = reqwest::Client::builder()
        .connect_timeout(crate::audio::diarization::DOWNLOAD_CONNECT_TIMEOUT)
        .build()?;
    let total = bundle.total_bytes();
    let mut done: u64 = 0;
    let mut eta = crate::download_eta::DownloadEta::new();

    for file in bundle.files.iter() {
        let destination = dir.join(file.name);
        if std::fs::metadata(&destination)
            .map(|m| m.len() == file.bytes)
            .unwrap_or(false)
        {
            done += file.bytes;
            continue;
        }
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let _ = tokio::fs::remove_file(&destination).await;
        info!(
            "Live model: downloading {} ({} bytes)",
            file.name, file.bytes
        );
        let done_before = done;
        crate::audio::diarization::download_verified(
            &client,
            &bundle.file_url(file.name),
            &destination,
            file.bytes,
            file.sha256,
            |percent| {
                let downloaded = overall_bytes(done_before, file.bytes, percent);
                eta.observe(downloaded, Instant::now());
                emit(
                    app,
                    DownloadProgressEvent {
                        model_name: bundle.id,
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

/// Download the live model the engine setting needs and wait for it. Emits
/// progress; a second caller while a download runs gets an error instead of
/// a duplicate download. A Core ML download is then compiled for the device
/// in the background, so the first recording starts quickly.
pub async fn download<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let bundle = needed_bundle(app);
    if bundle.is_downloaded(app) {
        emit(app, terminal_event(bundle, "completed", None));
        return Ok(());
    }
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err(anyhow!("The live model is already downloading"));
    }
    let task_app = app.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = tauri::async_runtime::spawn(async move {
        let result = download_all(&task_app, bundle).await;
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
            info!("Live model {} downloaded and verified", bundle.id);
            emit(app, terminal_event(bundle, "completed", None));
            // By id: `COREML` is a const, so its address is not stable.
            if bundle.id == COREML.id {
                super::engine::spawn_prepare(app.clone(), false);
            }
        }
        Err(error) if error.to_string() == "cancelled" => {
            remove_partials(app, bundle);
            emit(app, terminal_event(bundle, "cancelled", None));
        }
        Err(error) => {
            warn!("Live model download failed: {error:#}");
            emit(
                app,
                terminal_event(bundle, "error", Some(format!("{error:#}"))),
            );
        }
    }
    result
}

fn remove_partials<R: Runtime>(app: &AppHandle<R>, bundle: &Bundle) {
    if let Ok(dir) = bundle.dir(app) {
        for file in bundle.files.iter() {
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

/// Delete the downloaded live model (both bundles).
pub fn delete<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    super::engine::unload();
    for bundle in [&SHERPA, &COREML] {
        let dir = bundle.dir(app)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_pinned_to_the_revision() {
        for bundle in [&SHERPA, &COREML] {
            let url = bundle.file_url(bundle.files[0].name);
            assert!(url.contains(bundle.revision), "{url}");
            assert!(!url.contains("/main/"));
        }
        assert!(SHERPA.file_url(ENCODER).ends_with("/encoder.int8.onnx"));
        assert!(COREML
            .file_url("encoder/encoder_int8.mlmodelc/weights/weight.bin")
            .ends_with("/resolve/e673531caa6d25ab7baf5a8c14c9b99ba1551838/nemotron_coreml_560ms/encoder/encoder_int8.mlmodelc/weights/weight.bin"));
    }

    #[test]
    fn every_file_has_a_sha256_and_size() {
        for bundle in [&SHERPA, &COREML] {
            for file in bundle.files.iter() {
                assert_eq!(file.sha256.len(), 64, "{}", file.name);
                assert!(file.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(file.bytes > 0);
            }
            assert!(bundle.total_bytes() > 550 * 1024 * 1024);
        }
    }

    #[test]
    fn the_core_ml_bundle_has_what_the_helper_loads() {
        // nemotron-helper's NemotronModel opens these.
        for needed in [
            "metadata.json",
            "tokenizer.json",
            "encoder/encoder_int8.mlmodelc/",
            "decoder.mlmodelc/",
            "joint.mlmodelc/",
        ] {
            assert!(
                COREML
                    .files
                    .iter()
                    .any(|file| file.name.starts_with(needed)),
                "{needed}"
            );
        }
        for model in ["encoder/encoder_int8", "decoder", "joint"] {
            for part in [
                "coremldata.bin",
                "model.mil",
                "weights/weight.bin",
                "analytics/coremldata.bin",
            ] {
                let name = format!("{model}.mlmodelc/{part}");
                assert!(COREML.files.iter().any(|f| f.name == name), "{name}");
            }
        }
    }

    #[test]
    fn the_cpu_runs_the_sherpa_bundle_and_everything_else_core_ml() {
        assert_eq!(bundle_for(LiveEngineChoice::Cpu).id, SHERPA.id);
        for choice in [
            LiveEngineChoice::Auto,
            LiveEngineChoice::Gpu,
            LiveEngineChoice::NeuralEngine,
        ] {
            assert_eq!(bundle_for(choice).id, COREML.id);
        }
    }

    #[test]
    fn readiness_requires_every_file_at_its_exact_size() {
        for bundle in [&SHERPA, &COREML] {
            let dir = tempfile::tempdir().unwrap();
            assert!(!bundle.is_downloaded_in(dir.path()));
            for file in bundle.files.iter() {
                let path = dir.path().join(file.name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::File::create(&path)
                    .unwrap()
                    .set_len(file.bytes)
                    .unwrap();
            }
            assert!(bundle.is_downloaded_in(dir.path()));
            std::fs::File::create(dir.path().join(bundle.files[0].name))
                .unwrap()
                .set_len(1)
                .unwrap();
            assert!(!bundle.is_downloaded_in(dir.path()));
        }
    }

    #[test]
    fn overall_progress_counts_finished_files_and_the_one_in_flight() {
        assert_eq!(overall_bytes(0, 200, 50), 100);
        assert_eq!(overall_bytes(1_000, 200, 100), 1_200);
        assert_eq!(overall_bytes(1_000, 200, 150), 1_200);
    }
}
