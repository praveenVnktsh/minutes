//! The FluidAudio Core ML models the `speech-helper` sidecar loads, and how
//! the app downloads them. The helper runs offline and never touches the
//! network: the app fetches each model from Hugging Face at a pinned revision,
//! verifies it, and lays it out the way FluidAudio expects when it is given a
//! directory:
//!
//! ```text
//! <app_data_dir>/models/fluidaudio/        (`dir`, the helper's --models DIR)
//!   nemotron-speech-streaming-en-0.6b-coreml/nemotron_coreml_560ms/...  (live)
//!   parakeet-tdt-0.6b-v2-coreml/...                       (transcript after meetings)
//!   silero-vad-coreml/...                                 (speech detection)
//!   speaker-diarization-coreml/...                        (speaker labels)
//! ```
//!
//! Each folder is named after the last path component of its repo and holds
//! the files the helper needs at their repo-relative paths. Which files those
//! are comes from the repo tree at the pinned revision, filtered by
//! [`Bundle::include`]; sizes and SHA-256s come from the same tree, so nothing
//! here lists files by hand.
//!
//! A bundle counts as downloaded once `.minutes-complete` (holding the
//! revision) sits in its folder; it is written after every file is verified.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_store::StoreExt;
use tokio::io::AsyncWriteExt;

use super::helper::{self, Compute, Role};
use crate::audio::transcription_settings::{load_settings, LiveEngineChoice};
use crate::download_eta::DownloadEta;

/// Event carrying download progress to the frontend.
pub const DOWNLOAD_PROGRESS_EVENT: &str = "speech-model-download-progress";

/// Written last into a bundle's folder, holding the revision it came from.
const COMPLETE_MARKER: &str = ".minutes-complete";
/// FluidAudio's own revision marker. For a repo FluidAudio pins to a commit,
/// it refuses (offline) a cache without this file naming that commit.
const FLUIDAUDIO_REVISION_MARKER: &str = ".fluidaudio-revision";

/// Set when the user cancels a download, so it is not restarted
/// automatically on every launch. Cleared by an explicit download.
pub const AUTO_DOWNLOAD_DISABLED_KEY: &str = "speechModelsAutoDownloadDisabled";
const STORE_FILE: &str = "store.json";

const HF: &str = "https://huggingface.co";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Give up on a stalled download after this long without data.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const TREE_TIMEOUT: Duration = Duration::from_secs(60);
/// Progress events go out when the percentage changes, or this often.
const EMIT_INTERVAL: Duration = Duration::from_millis(500);
const MB: f64 = 1024.0 * 1024.0;

/// What a model is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelKind {
    LiveAsr,
    FinalAsr,
    Vad,
    Diarization,
}

impl ModelKind {
    /// Every kind, in download order: what the transcript after a meeting
    /// needs first, then speaker labels, then the live model (the largest).
    pub const ALL: [ModelKind; 4] = [
        ModelKind::FinalAsr,
        ModelKind::Vad,
        ModelKind::Diarization,
        ModelKind::LiveAsr,
    ];
}

/// One downloadable model: a Hugging Face repo pinned to a revision.
#[derive(Debug)]
pub struct Bundle {
    /// Identifier shown in settings; the same as `dir_name`.
    pub id: &'static str,
    pub kind: ModelKind,
    pub repo: &'static str,
    /// Pinned Hugging Face revision of `repo`.
    pub revision: &'static str,
    /// Folder under [`dir`]: the last path component of `repo`.
    pub dir_name: &'static str,
    pub display_name: &'static str,
    /// Size of the files [`Bundle::include`] selects at `revision`, for the
    /// UI before the download starts.
    pub approx_bytes: u64,
    /// The repo paths the helper loads: an entry ending in `/` is a folder,
    /// anything else a file. Loose `.json`/`.txt` files at the repo root
    /// always come along (configs and vocabularies; FluidAudio's own download
    /// sweeps them in the same way). The repos also carry other variants and
    /// source packages, gigabytes the app never loads.
    pub include: &'static [&'static str],
    /// FluidAudio 0.18.0 pins this repo to `revision` itself, so the folder
    /// also gets FluidAudio's revision marker.
    pub fluidaudio_pinned: bool,
}

const LIVE_ASR: Bundle = Bundle {
    id: "nemotron-speech-streaming-en-0.6b-coreml",
    kind: ModelKind::LiveAsr,
    repo: "FluidInference/nemotron-speech-streaming-en-0.6b-coreml",
    revision: "e673531caa6d25ab7baf5a8c14c9b99ba1551838",
    dir_name: "nemotron-speech-streaming-en-0.6b-coreml",
    display_name: "Nemotron Speech Streaming 0.6B",
    approx_bytes: 626_449_024,
    // The repo holds several chunk sizes; live text uses 560 ms chunks
    // (FluidAudio's `NemotronChunkSize.ms560`), loaded from this folder.
    include: &["nemotron_coreml_560ms/"],
    fluidaudio_pinned: false,
};

const FINAL_ASR: Bundle = Bundle {
    id: "parakeet-tdt-0.6b-v2-coreml",
    kind: ModelKind::FinalAsr,
    repo: "FluidInference/parakeet-tdt-0.6b-v2-coreml",
    revision: "ee09c569f73759e6d44c9bd16766f477b2b36d39",
    dir_name: "parakeet-tdt-0.6b-v2-coreml",
    display_name: "Parakeet TDT 0.6B v2 (English)",
    approx_bytes: 464_413_250,
    // `ModelNames.ASR.requiredModels` plus `parakeet_vocab.json` (a root file).
    include: &[
        "Preprocessor.mlmodelc/",
        "Encoder.mlmodelc/",
        "Decoder.mlmodelc/",
        "JointDecision.mlmodelc/",
        "parakeet_vocab.json",
    ],
    fluidaudio_pinned: false,
};

const VAD: Bundle = Bundle {
    id: "silero-vad-coreml",
    kind: ModelKind::Vad,
    repo: "FluidInference/silero-vad-coreml",
    revision: "b419383c55c110e2c9271fa6ee0ea83d03c70d96",
    dir_name: "silero-vad-coreml",
    display_name: "Silero VAD",
    approx_bytes: 1_063_427,
    // `ModelNames.VAD.requiredModels`.
    include: &["silero-vad-unified-256ms-v6.2.1.mlmodelc/"],
    fluidaudio_pinned: false,
};

const DIARIZATION: Bundle = Bundle {
    id: "speaker-diarization-coreml",
    kind: ModelKind::Diarization,
    repo: "FluidInference/speaker-diarization-coreml",
    // The commit FluidAudio 0.18.0 pins (`Repo.diarizer.revision`).
    revision: "df2625ac79a7ac6b65ad868fee6d80f320da4232",
    dir_name: "speaker-diarization-coreml",
    display_name: "Pyannote community-1 speaker diarization",
    approx_bytes: 21_786_966,
    // `ModelNames.OfflineDiarizer.requiredModels` (`plda-parameters.json` is
    // a root file).
    include: &[
        "Segmentation.mlmodelc/",
        "FBank.mlmodelc/",
        "Embedding.mlmodelc/",
        "PldaRho.mlmodelc/",
        "plda-parameters.json",
    ],
    fluidaudio_pinned: true,
};

pub const BUNDLES: [Bundle; 4] = [LIVE_ASR, FINAL_ASR, VAD, DIARIZATION];

/// The bundle of `kind`.
pub fn bundle(kind: ModelKind) -> &'static Bundle {
    match kind {
        ModelKind::LiveAsr => &LIVE_ASR,
        ModelKind::FinalAsr => &FINAL_ASR,
        ModelKind::Vad => &VAD,
        ModelKind::Diarization => &DIARIZATION,
    }
}

impl Bundle {
    pub fn tree_url(&self) -> String {
        format!(
            "{HF}/api/models/{}/tree/{}?recursive=true",
            self.repo, self.revision
        )
    }

    pub fn file_url(&self, path: &str) -> String {
        format!(
            "{HF}/{}/resolve/{}/{}",
            self.repo,
            self.revision,
            encode_path(path)
        )
    }

    /// Whether the helper loads `path` (relative to the repo root).
    pub fn wants(&self, path: &str) -> bool {
        let at_root_metadata =
            !path.contains('/') && (path.ends_with(".json") || path.ends_with(".txt"));
        at_root_metadata
            || self
                .include
                .iter()
                .any(|include| match include.strip_suffix('/') {
                    Some(_) => path.starts_with(include),
                    None => path == *include,
                })
    }

    /// Whether the bundle is complete under the models folder `root`.
    pub fn is_downloaded_in(&self, root: &Path) -> bool {
        std::fs::read_to_string(root.join(self.dir_name).join(COMPLETE_MARKER))
            .map(|revision| revision.trim() == self.revision)
            .unwrap_or(false)
    }
}

/// The folder the helper is given as `--models`.
pub fn dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("models").join("fluidaudio"))
}

pub fn is_downloaded<R: Runtime>(app: &AppHandle<R>, kind: ModelKind) -> bool {
    dir(app)
        .map(|root| bundle(kind).is_downloaded_in(&root))
        .unwrap_or(false)
}

/// Whether live text can run: the live model is on disk and the helper exists
/// (never below macOS 15, see [`helper::os_supported`]).
pub fn live_ready<R: Runtime>(app: &AppHandle<R>) -> bool {
    is_downloaded(app, ModelKind::LiveAsr) && helper::resolve_helper().is_some()
}

/// Whether a meeting can be transcribed afterwards. Speaker labels are
/// optional: without their model a transcript simply has none.
pub fn batch_ready<R: Runtime>(app: &AppHandle<R>) -> bool {
    helper::os_supported()
        && is_downloaded(app, ModelKind::FinalAsr)
        && is_downloaded(app, ModelKind::Vad)
}

/// Whether speaker labels can run.
pub fn diarization_ready<R: Runtime>(app: &AppHandle<R>) -> bool {
    helper::os_supported() && is_downloaded(app, ModelKind::Diarization)
}

/// The kinds the transcript after a meeting needs.
pub const BATCH_KINDS: [ModelKind; 2] = [ModelKind::FinalAsr, ModelKind::Vad];

/// Whether models a job needs are on disk, on their way, or not coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Ready,
    /// Missing, but downloading now or on the next start (automatic
    /// downloads are on): work that needs them waits instead of giving up.
    Coming,
    /// Missing for good: macOS older than 15, or the user cancelled the
    /// automatic download and has not downloaded them since.
    Unavailable,
}

/// The pure rule behind [`availability`].
pub fn availability_of(
    os_supported: bool,
    downloaded: bool,
    downloading: bool,
    auto_download_disabled: bool,
) -> Availability {
    if !os_supported {
        Availability::Unavailable
    } else if downloaded {
        Availability::Ready
    } else if downloading || !auto_download_disabled {
        Availability::Coming
    } else {
        Availability::Unavailable
    }
}

/// Whether every one of `kinds` is on disk, on its way, or not coming.
pub fn availability<R: Runtime>(app: &AppHandle<R>, kinds: &[ModelKind]) -> Availability {
    availability_of(
        helper::os_supported(),
        kinds.iter().all(|kind| is_downloaded(app, *kind)),
        kinds.iter().any(|kind| is_downloading_kind(*kind)),
        auto_download_disabled(app),
    )
}

// ---------------------------------------------------------------------------
// The repo tree
// ---------------------------------------------------------------------------

/// One entry of `GET /api/models/{repo}/tree/{revision}?recursive=true`.
#[derive(Debug, Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    lfs: Option<LfsInfo>,
}

#[derive(Debug, Deserialize)]
struct LfsInfo {
    /// SHA-256 of the file's content.
    oid: String,
    size: u64,
}

/// A file to download, relative to the bundle's folder (and the repo root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: u64,
    /// LFS files carry a SHA-256; the rest are small git files, checked by
    /// size.
    pub sha256: Option<String>,
}

/// The files of `bundle` among `entries`. Fails if an `include` entry
/// matches nothing (the repo no longer has what the helper loads) or a path
/// could escape the bundle's folder.
fn manifest(bundle: &Bundle, entries: Vec<TreeEntry>) -> Result<Vec<ManifestFile>> {
    let mut files = Vec::new();
    for entry in entries {
        if entry.kind != "file" || !bundle.wants(&entry.path) {
            continue;
        }
        if !is_safe_relative(&entry.path) {
            bail!("Unexpected file path in {}: {}", bundle.repo, entry.path);
        }
        let (bytes, sha256) = match entry.lfs {
            Some(lfs) => {
                if lfs.oid.len() != 64 || !lfs.oid.chars().all(|c| c.is_ascii_hexdigit()) {
                    bail!("Unexpected checksum for {} in {}", entry.path, bundle.repo);
                }
                (lfs.size, Some(lfs.oid.to_ascii_lowercase()))
            }
            None => (entry.size, None),
        };
        files.push(ManifestFile {
            path: entry.path,
            bytes,
            sha256,
        });
    }
    for include in bundle.include {
        let found = files.iter().any(|file| match include.strip_suffix('/') {
            Some(_) => file.path.starts_with(include),
            None => file.path == *include,
        });
        if !found {
            bail!("{} at {} has no {include}", bundle.repo, bundle.revision);
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn parse_tree(json: &str) -> Result<Vec<TreeEntry>> {
    serde_json::from_str(json).context("Unexpected Hugging Face tree listing")
}

/// The `rel="next"` URL of a paginated listing's `Link` header.
fn next_page(link: Option<&str>) -> Option<String> {
    link?.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|param| param.trim() == "rel=\"next\"")
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            })
    })
}

async fn fetch_manifest(client: &reqwest::Client, bundle: &Bundle) -> Result<Vec<ManifestFile>> {
    let mut entries = Vec::new();
    let mut url = Some(bundle.tree_url());
    while let Some(page) = url.take() {
        let response = client
            .get(&page)
            .timeout(TREE_TIMEOUT)
            .send()
            .await
            .with_context(|| format!("Failed to list the files of {}", bundle.repo))?
            .error_for_status()
            .with_context(|| format!("Failed to list the files of {}", bundle.repo))?;
        url = next_page(
            response
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|value| value.to_str().ok()),
        );
        entries.extend(parse_tree(&response.text().await?)?);
    }
    manifest(bundle, entries)
}

/// A repo path that stays inside the folder it is joined to.
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Percent-encode a repo path for a URL, keeping the `/` separators.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static DOWNLOAD_TASK: Mutex<Option<tauri::async_runtime::JoinHandle<()>>> = Mutex::new(None);
/// The kinds the running download has not finished yet.
static QUEUE: Mutex<Vec<ModelKind>> = Mutex::new(Vec::new());

pub fn is_downloading() -> bool {
    DOWNLOADING.load(Ordering::SeqCst)
}

/// Whether the running download includes `kind` and has not finished it.
pub fn is_downloading_kind(kind: ModelKind) -> bool {
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&kind)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgressEvent {
    pub model_id: String,
    pub kind: ModelKind,
    /// Overall for the bundle, 0-100.
    pub progress: u32,
    pub downloaded_mb: f64,
    pub total_mb: f64,
    pub speed_mbps: f64,
    pub eta_seconds: Option<u64>,
    /// downloading | completed | error | cancelled
    pub status: &'static str,
    pub error: Option<String>,
}

fn emit<R: Runtime>(app: &AppHandle<R>, event: DownloadProgressEvent) {
    if let Err(error) = app.emit(DOWNLOAD_PROGRESS_EVENT, event) {
        warn!("Failed to emit speech model download progress: {error}");
    }
}

fn terminal_event(
    bundle: &'static Bundle,
    status: &'static str,
    error: Option<String>,
) -> DownloadProgressEvent {
    let total = bundle.approx_bytes as f64 / MB;
    let completed = status == "completed";
    DownloadProgressEvent {
        model_id: bundle.id.to_string(),
        kind: bundle.kind,
        progress: if completed { 100 } else { 0 },
        downloaded_mb: if completed { total } else { 0.0 },
        total_mb: total,
        speed_mbps: 0.0,
        eta_seconds: None,
        status,
        error,
    }
}

/// Overall progress after `done_bytes` of finished files plus `received` of
/// the `file_bytes` file in flight.
pub fn overall_bytes(done_bytes: u64, file_bytes: u64, received: u64) -> u64 {
    done_bytes + received.min(file_bytes)
}

fn percent(downloaded: u64, total: u64) -> u32 {
    (downloaded.min(total) * 100 / total.max(1)) as u32
}

/// Turns byte counts into throttled progress events for one bundle.
struct Reporter<'a, R: Runtime> {
    app: &'a AppHandle<R>,
    bundle: &'static Bundle,
    total: u64,
    eta: DownloadEta,
    last: Option<(u32, Instant)>,
}

impl<R: Runtime> Reporter<'_, R> {
    fn report(&mut self, downloaded: u64) {
        let now = Instant::now();
        let progress = percent(downloaded, self.total);
        if let Some((last_progress, at)) = self.last {
            if last_progress == progress && now.duration_since(at) < EMIT_INTERVAL {
                return;
            }
        }
        self.last = Some((progress, now));
        self.eta.observe(downloaded, now);
        emit(
            self.app,
            DownloadProgressEvent {
                model_id: self.bundle.id.to_string(),
                kind: self.bundle.kind,
                progress,
                downloaded_mb: downloaded as f64 / MB,
                total_mb: self.total as f64 / MB,
                speed_mbps: self.eta.bytes_per_second().unwrap_or(0.0) / MB,
                eta_seconds: self.eta.seconds_remaining(downloaded, self.total),
                status: "downloading",
                error: None,
            },
        );
    }
}

/// Where a file is written while it downloads. A suffix rather than a new
/// extension, so `model.mil` and `model.bin` cannot share one.
fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Stream `url` into `destination`, checking its size (and SHA-256 when
/// known) before it is moved into place, so `destination` only ever exists
/// verified. `on_bytes` gets the bytes received so far.
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_bytes: u64,
    expected_sha256: Option<&str>,
    on_bytes: impl FnMut(u64),
) -> Result<()> {
    let partial = partial_path(destination);
    let outcome = stream_to_file(
        client,
        url,
        &partial,
        destination,
        expected_bytes,
        expected_sha256,
        on_bytes,
    )
    .await;
    if outcome.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    outcome
}

async fn stream_to_file(
    client: &reqwest::Client,
    url: &str,
    partial: &Path,
    destination: &Path,
    expected_bytes: u64,
    expected_sha256: Option<&str>,
    mut on_bytes: impl FnMut(u64),
) -> Result<()> {
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to request {url}"))?
        .error_for_status()?;
    let file = tokio::fs::File::create(partial)
        .await
        .with_context(|| format!("Failed to create {}", partial.display()))?;
    let mut writer = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    loop {
        let chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk())
            .await
            .map_err(|_| anyhow!("Download stalled: no data for {}s", READ_TIMEOUT.as_secs()))??;
        let Some(chunk) = chunk else { break };
        received += chunk.len() as u64;
        if received > expected_bytes {
            bail!("Download is larger than the expected {expected_bytes} bytes");
        }
        if expected_sha256.is_some() {
            hasher.update(&chunk);
        }
        writer.write_all(&chunk).await?;
        on_bytes(received);
    }
    writer.flush().await?;
    writer.get_ref().sync_all().await?;
    drop(writer);

    if received != expected_bytes {
        bail!("Download is incomplete: received {received} of {expected_bytes} bytes");
    }
    if let Some(expected) = expected_sha256 {
        let digest = hex::encode(hasher.finalize());
        if !digest.eq_ignore_ascii_case(expected) {
            bail!("Download checksum mismatch: expected {expected}, got {digest}");
        }
    }
    tokio::fs::rename(partial, destination)
        .await
        .with_context(|| format!("Failed to move model file into {}", destination.display()))?;
    Ok(())
}

/// Whether a file left by an earlier, unfinished download can be kept: the
/// right size and, being an LFS file, the right SHA-256. Small git files are
/// always fetched again, so a resumed or upgraded bundle never mixes
/// revisions.
async fn reusable(path: &Path, file: &ManifestFile) -> bool {
    let Some(expected) = file.sha256.clone() else {
        return false;
    };
    let size_matches = tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len() == file.bytes)
        .unwrap_or(false);
    if !size_matches {
        return false;
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .ok()
        .and_then(|digest| digest.ok())
        .map(|digest| digest.eq_ignore_ascii_case(&expected))
        .unwrap_or(false)
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Remove files under `dir` that are not in `keep` (left by an older
/// revision), and folders that end up empty.
fn prune(dir: &Path, keep: &HashSet<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            prune(&path, keep)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else if !keep.contains(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Download every file of `bundle` into its folder under `root`, then mark it
/// complete.
async fn download_bundle<R: Runtime>(
    app: &AppHandle<R>,
    client: &reqwest::Client,
    root: &Path,
    bundle: &'static Bundle,
) -> Result<()> {
    let folder = root.join(bundle.dir_name);
    tokio::fs::create_dir_all(&folder).await?;
    // An older revision's markers go first, so a half-upgraded folder never
    // counts as complete.
    for marker in [COMPLETE_MARKER, FLUIDAUDIO_REVISION_MARKER] {
        let _ = tokio::fs::remove_file(folder.join(marker)).await;
    }

    let files = fetch_manifest(client, bundle).await?;
    if bundle.kind == ModelKind::LiveAsr {
        if let Some(models) = root.parent() {
            adopt_retired_live_files(&models.join(RETIRED_LIVE_COREML_DIR), &folder, &files);
        }
    }
    let total: u64 = files.iter().map(|file| file.bytes).sum();
    let mut reporter = Reporter {
        app,
        bundle,
        total,
        eta: DownloadEta::new(),
        last: None,
    };
    reporter.report(0);

    let mut done: u64 = 0;
    for file in &files {
        let destination = folder.join(&file.path);
        if reusable(&destination, file).await {
            done += file.bytes;
            reporter.report(done);
            continue;
        }
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let _ = tokio::fs::remove_file(&destination).await;
        info!(
            "Speech models: downloading {}/{} ({} bytes)",
            bundle.dir_name, file.path, file.bytes
        );
        let done_before = done;
        download_file(
            client,
            &bundle.file_url(&file.path),
            &destination,
            file.bytes,
            file.sha256.as_deref(),
            |received| reporter.report(overall_bytes(done_before, file.bytes, received)),
        )
        .await
        .with_context(|| format!("Failed to download {} from {}", file.path, bundle.repo))?;
        done += file.bytes;
    }

    let keep: HashSet<PathBuf> = files.iter().map(|file| folder.join(&file.path)).collect();
    if let Err(error) = prune(&folder, &keep) {
        warn!(
            "Speech models: could not tidy {}: {error}",
            folder.display()
        );
    }
    if bundle.fluidaudio_pinned {
        tokio::fs::write(
            folder.join(FLUIDAUDIO_REVISION_MARKER),
            format!("{}\n", bundle.revision),
        )
        .await?;
    }
    tokio::fs::write(
        folder.join(COMPLETE_MARKER),
        format!("{}\n", bundle.revision),
    )
    .await?;
    Ok(())
}

/// The kinds among `kinds` still to download, in download order.
fn missing_in_order(kinds: &[ModelKind], downloaded: impl Fn(ModelKind) -> bool) -> Vec<ModelKind> {
    ModelKind::ALL
        .into_iter()
        .filter(|kind| kinds.contains(kind) && !downloaded(*kind))
        .collect()
}

fn take_from_queue(kind: ModelKind) {
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|queued| *queued != kind);
}

/// Download the missing models among `kinds`, one after another, and wait
/// for them. Each bundle reports its own progress; a bundle that fails does
/// not stop the others, and the first failure is returned. A second caller
/// while a download runs gets an error instead of a duplicate download.
pub async fn download<R: Runtime>(app: &AppHandle<R>, kinds: &[ModelKind]) -> Result<()> {
    if !helper::os_supported() {
        bail!(helper::OS_UNSUPPORTED_MESSAGE);
    }
    let root = dir(app)?;
    let missing = missing_in_order(kinds, |kind| bundle(kind).is_downloaded_in(&root));
    for kind in kinds {
        if !missing.contains(kind) {
            emit(app, terminal_event(bundle(*kind), "completed", None));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let receiver = {
        // Held until the task's handle is stored, so `cancel_download` never
        // sees a download that has started but cannot be aborted yet.
        let mut task = DOWNLOAD_TASK.lock().unwrap_or_else(|e| e.into_inner());
        if DOWNLOADING.swap(true, Ordering::SeqCst) {
            return Err(anyhow!("The speech models are already downloading"));
        }
        *QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = missing.clone();

        let task_app = app.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *task = Some(tauri::async_runtime::spawn(async move {
            let result = download_queue(&task_app, &root, &missing).await;
            let _ = sender.send(result);
        }));
        receiver
    };

    let result = match receiver.await {
        Ok(result) => result,
        // The task was aborted by `cancel_download`.
        Err(_) => {
            let cancelled = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
            for kind in cancelled {
                remove_partials(app, bundle(kind));
                emit(app, terminal_event(bundle(kind), "cancelled", None));
            }
            Err(anyhow!("cancelled"))
        }
    };
    {
        // Under the task lock, so a download starting right after this one
        // cannot have its handle taken here.
        let mut task = DOWNLOAD_TASK.lock().unwrap_or_else(|e| e.into_inner());
        task.take();
        QUEUE.lock().unwrap_or_else(|e| e.into_inner()).clear();
        DOWNLOADING.store(false, Ordering::SeqCst);
    }
    result
}

async fn download_queue<R: Runtime>(
    app: &AppHandle<R>,
    root: &Path,
    kinds: &[ModelKind],
) -> Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()?;
    let mut first_error = None;
    for &kind in kinds {
        let bundle = bundle(kind);
        let result = download_bundle(app, &client, root, bundle).await;
        take_from_queue(kind);
        match result {
            Ok(()) => {
                info!("Speech model {} downloaded and verified", bundle.id);
                emit(app, terminal_event(bundle, "completed", None));
                if kind == ModelKind::LiveAsr {
                    // Compile it for the device now, so the first recording
                    // starts quickly.
                    spawn_live_prepare(app, false);
                } else {
                    on_models_ready(app);
                }
            }
            Err(error) => {
                warn!("Speech model {} download failed: {error:#}", bundle.id);
                remove_partials(app, bundle);
                emit(
                    app,
                    terminal_event(bundle, "error", Some(format!("{error:#}"))),
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// A batch model (transcript, VAD or speakers) finished downloading: run the
/// work that waited for it. Final passes and speaker labels recorded while
/// the models were still on their way stay pending until now.
fn on_models_ready<R: Runtime>(app: &AppHandle<R>) {
    if batch_ready(app) {
        crate::audio::final_pass::spawn_waiting_for_models(app);
    }
    if diarization_ready(app) {
        crate::audio::diarization::spawn_resume(app);
    }
}

/// Remove the `.part` files an interrupted download left in `bundle`'s folder.
fn remove_partials<R: Runtime>(app: &AppHandle<R>, bundle: &Bundle) {
    fn walk(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                walk(&path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "part")
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    if let Ok(root) = dir(app) {
        walk(&root.join(bundle.dir_name));
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

/// Delete a downloaded model. Callers stop whatever has it loaded first.
pub fn delete<R: Runtime>(app: &AppHandle<R>, kind: ModelKind) -> Result<()> {
    if is_downloading_kind(kind) {
        bail!("{} is downloading", bundle(kind).display_name);
    }
    let folder = dir(app)?.join(bundle(kind).dir_name);
    if folder.exists() {
        std::fs::remove_dir_all(&folder)
            .with_context(|| format!("Failed to delete {}", folder.display()))?;
    }
    info!("Speech model {} deleted", bundle(kind).id);
    Ok(())
}

// ---------------------------------------------------------------------------
// Models of the removed engines
// ---------------------------------------------------------------------------

/// Where the previous live engine kept its Core ML model, under
/// `<app_data_dir>/models`: the same repo and revision as [`LIVE_ASR`], with
/// the files of its `nemotron_coreml_560ms/` folder at the top.
const RETIRED_LIVE_COREML_DIR: &str = "nemotron-streaming-coreml";
/// The folder of [`LIVE_ASR`]'s repo the old live model was flattened from.
const LIVE_SUBDIR: &str = "nemotron_coreml_560ms/";

/// Set once everything the removed engines downloaded is gone.
const RETIRED_MODELS_REMOVED_KEY: &str = "retiredSpeechModelsRemoved";

/// Folders the removed engines created under `<app_data_dir>/models`. The
/// models folder also holds the summary models (`summary/`) and this store
/// (`fluidaudio/`), so only these exact names are removed.
const RETIRED_DIRS: &[&str] = &[
    // Parakeet ONNX (parakeet_engine: `models/parakeet/<model>/`).
    "parakeet",
    // sherpa-onnx Nemotron live model (live_streaming/model.rs `SHERPA`).
    "nemotron-streaming",
    // Nemotron-3 Sortformer, sherpa pyannote segmentation and WeSpeaker
    // (diarization.rs: `models/diarization/...`).
    "diarization",
    // Whisper Core ML encoders and their unpack staging folders
    // (whisper_engine/coreml_encoder.rs).
    "ggml-large-v3-turbo-encoder.mlmodelc",
    "ggml-large-v3-encoder.mlmodelc",
    "ggml-large-v3-turbo-encoder.mlmodelc.unpacking",
    "ggml-large-v3-encoder.mlmodelc.unpacking",
];

/// Files the removed engines created under `<app_data_dir>/models`: the
/// Whisper models of `WHISPER_MODEL_CATALOG` and the encoder archives.
const RETIRED_FILES: &[&str] = &[
    "ggml-tiny.bin",
    "ggml-base.bin",
    "ggml-small.bin",
    "ggml-medium.bin",
    "ggml-large-v3-turbo.bin",
    "ggml-large-v3.bin",
    "ggml-tiny-q5_1.bin",
    "ggml-base-q5_1.bin",
    "ggml-small-q5_1.bin",
    "ggml-medium-q5_0.bin",
    "ggml-large-v3-turbo-q5_0.bin",
    "ggml-large-v3-q5_0.bin",
    "ggml-large-v3-turbo-encoder.mlmodelc.zip",
    "ggml-large-v3-encoder.mlmodelc.zip",
];

/// Move the old live model's files (`old`) into [`LIVE_ASR`]'s folder, so the
/// download reuses them instead of fetching 626 MB again. Only files of the
/// manifest move, and only where nothing is in the way; [`reusable`] still
/// checks each one's SHA-256 before it is kept. Same volume, so a rename.
fn adopt_retired_live_files(old: &Path, folder: &Path, files: &[ManifestFile]) {
    if !old.is_dir() {
        return;
    }
    let (mut moved, mut bytes) = (0usize, 0u64);
    for file in files {
        let Some(relative) = file.path.strip_prefix(LIVE_SUBDIR) else {
            continue;
        };
        let source = old.join(relative);
        let destination = folder.join(&file.path);
        if !source.is_file() || destination.exists() {
            continue;
        }
        let moved_one = destination
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::rename(&source, &destination));
        match moved_one {
            Ok(()) => {
                moved += 1;
                bytes += file.bytes;
            }
            Err(error) => warn!(
                "Speech models: could not move {} into {}: {error}",
                source.display(),
                destination.display()
            ),
        }
    }
    if moved > 0 {
        info!(
            "Speech models: reusing {moved} file(s) ({:.0} MB) of the previous live model from {}",
            bytes as f64 / MB,
            old.display()
        );
    }
}

/// The retired model paths present under `models` (`<app_data_dir>/models`).
/// The old live model is left out while `keep_old_live`: its files are moved
/// into the new store when the live model downloads.
fn retired_paths(models: &Path, keep_old_live: bool) -> Vec<PathBuf> {
    let old_live = (!keep_old_live).then_some(RETIRED_LIVE_COREML_DIR);
    RETIRED_DIRS
        .iter()
        .chain(RETIRED_FILES)
        .copied()
        .chain(old_live)
        .map(|name| models.join(name))
        .filter(|path| path.symlink_metadata().is_ok())
        .collect()
}

/// Bytes under `path`, without following links.
fn disk_bytes(path: &Path) -> u64 {
    let Ok(metadata) = path.symlink_metadata() else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| disk_bytes(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

/// Delete the retired model paths under `models`. Returns each deleted path
/// with its size, and whether nothing is left to delete later.
fn remove_retired(models: &Path, keep_old_live: bool) -> (Vec<(PathBuf, u64)>, bool) {
    let mut removed = Vec::new();
    let mut complete = !(keep_old_live && models.join(RETIRED_LIVE_COREML_DIR).exists());
    for path in retired_paths(models, keep_old_live) {
        let bytes = disk_bytes(&path);
        let is_dir = path.symlink_metadata().is_ok_and(|m| m.is_dir());
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed.push((path, bytes)),
            Err(error) => {
                warn!(
                    "Speech models: could not delete the retired {}: {error}",
                    path.display()
                );
                complete = false;
            }
        }
    }
    (removed, complete)
}

/// Delete what the removed engines (Whisper, ONNX Parakeet, sherpa Nemotron,
/// the old diarizers) downloaded, once. The old Core ML live model waits
/// until the new live model is downloaded (its files move there first).
async fn retire_old_models<R: Runtime>(app: &AppHandle<R>) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    if store
        .get(RETIRED_MODELS_REMOVED_KEY)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return;
    }
    let Ok(models) = app.path().app_data_dir().map(|dir| dir.join("models")) else {
        return;
    };
    let keep_old_live = helper::os_supported() && !is_downloaded(app, ModelKind::LiveAsr);
    let Ok((removed, complete)) =
        tokio::task::spawn_blocking(move || remove_retired(&models, keep_old_live)).await
    else {
        return;
    };
    for (path, bytes) in &removed {
        info!(
            "Speech models: deleted the retired {} ({:.0} MB)",
            path.display(),
            *bytes as f64 / MB
        );
    }
    if !removed.is_empty() {
        let total: u64 = removed.iter().map(|(_, bytes)| bytes).sum();
        info!(
            "Speech models: retired models freed {:.0} MB",
            total as f64 / MB
        );
    }
    if complete {
        store.set(RETIRED_MODELS_REMOVED_KEY, serde_json::json!(true));
        if let Err(error) = store.save() {
            warn!("Could not save {RETIRED_MODELS_REMOVED_KEY}: {error}");
        }
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Create the models folder, point the batch helper at it, and fetch missing
/// models in the background once onboarding is done (now, or when it
/// finishes).
pub fn setup<R: Runtime>(app: &AppHandle<R>) {
    use tauri::Listener;

    match dir(app) {
        Ok(root) => {
            if let Err(error) = std::fs::create_dir_all(&root) {
                warn!(
                    "Speech models: could not create {}: {error}",
                    root.display()
                );
            }
            super::batch::configure(root);
        }
        Err(error) => warn!("Speech models: no app data folder: {error}"),
    }

    spawn_default_model_setup(app.clone());
    let handle = app.clone();
    app.listen(crate::onboarding::ONBOARDING_COMPLETED_EVENT, move |_| {
        spawn_default_model_setup(handle.clone());
    });
}

fn auto_download_disabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.store(STORE_FILE)
        .ok()
        .and_then(|store| store.get(AUTO_DOWNLOAD_DISABLED_KEY))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Apply the transcription defaults once onboarding is done: migrate the
/// settings, then download every missing model in the background unless the
/// user cancelled a download before. Downloads report progress through their
/// usual event, so the download toast and settings show them. Failures are
/// logged and retried on a later start.
fn spawn_default_model_setup<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        match crate::onboarding::load_onboarding_status(&app).await {
            Ok(status) if status.completed => {}
            Ok(_) => return, // Runs again when onboarding finishes.
            Err(error) => {
                warn!("Default speech model setup skipped: {error}");
                return;
            }
        }

        crate::audio::transcription_settings::migrate_settings(&app).await;
        retire_old_models(&app).await;

        if !helper::os_supported() {
            // Logged by os_supported; nothing to download or prepare.
            return;
        }
        let live_was_downloaded = is_downloaded(&app, ModelKind::LiveAsr);
        if !auto_download_disabled(&app) && !is_downloading() {
            let missing: Vec<ModelKind> = ModelKind::ALL
                .into_iter()
                .filter(|kind| !is_downloaded(&app, *kind))
                .collect();
            if !missing.is_empty() {
                info!("Downloading speech models in the background: {missing:?}");
                if let Err(error) = download(&app, &missing).await {
                    warn!("Background speech model download did not finish: {error:#}");
                }
                // The old live model's files have moved by now.
                retire_old_models(&app).await;
            }
        }
        if live_was_downloaded {
            // Keeps Core ML's compiled-model cache warm (it is rebuilt after
            // OS updates), so recordings start without the compile.
            spawn_live_prepare(&app, true);
        }
    });
}

/// Compile the live model for the device the live engine setting uses, on a
/// background thread. At startup the GPU is skipped: its shaders compile on
/// every launch anyway, so warming them up only costs time.
fn spawn_live_prepare<R: Runtime>(app: &AppHandle<R>, startup: bool) {
    let compute = match load_settings(app).live_engine {
        LiveEngineChoice::Gpu if startup => return,
        LiveEngineChoice::Gpu => Compute::Gpu,
        LiveEngineChoice::Auto | LiveEngineChoice::NeuralEngine => Compute::NeuralEngine,
    };
    let Ok(root) = dir(app) else {
        return;
    };
    if helper::resolve_helper().is_none() || !LIVE_ASR.is_downloaded_in(&root) {
        return;
    }
    std::thread::spawn(move || match helper::prepare(Role::Live, &root, compute) {
        Ok(seconds) => info!(
            "Nemotron Core ML ({}) prepared in {seconds:.1}s",
            compute.arg()
        ),
        Err(error) => warn!("Nemotron Core ML preparation failed: {error:#}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = r#"[
        {"type":"directory","oid":"a","size":0,"path":"nemotron_coreml_560ms"},
        {"type":"file","oid":"b","size":2,"path":"config.json"},
        {"type":"file","oid":"c","size":5908,"path":"README.md"},
        {"type":"file","oid":"d","size":8614,"path":"scripts/convert.py"},
        {"type":"file","oid":"e","size":438,"path":"nemotron_coreml_560ms/metadata.json"},
        {"type":"file","oid":"f","size":589057600,
         "lfs":{"oid":"8CED31411F7903EAEC52DE85E94C7847228F3E44028D8F0040800E5603943A40","size":589057600,"pointerSize":135},
         "path":"nemotron_coreml_560ms/encoder/encoder_int8.mlmodelc/weights/weight.bin"},
        {"type":"file","oid":"g","size":13093,"path":"nemotron_coreml_1120ms/decoder.mlmodelc/model.mil"},
        {"type":"file","oid":"h","size":441,"path":"nemotron_coreml_1120ms/metadata.json"}
    ]"#;

    #[test]
    fn the_manifest_keeps_the_included_folder_and_root_metadata() {
        let files = manifest(&LIVE_ASR, parse_tree(TREE).unwrap()).unwrap();
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "config.json",
                "nemotron_coreml_560ms/encoder/encoder_int8.mlmodelc/weights/weight.bin",
                "nemotron_coreml_560ms/metadata.json",
            ]
        );
        let weights = &files[1];
        assert_eq!(weights.bytes, 589_057_600);
        assert_eq!(
            weights.sha256.as_deref(),
            Some("8ced31411f7903eaec52de85e94c7847228f3e44028d8f0040800e5603943a40")
        );
        // A git file is checked by size only.
        assert_eq!(files[2].sha256, None);
        assert_eq!(files[2].bytes, 438);
    }

    #[test]
    fn a_repo_without_an_included_path_is_rejected() {
        let tree = r#"[{"type":"file","oid":"b","size":2,"path":"config.json"}]"#;
        let error = manifest(&FINAL_ASR, parse_tree(tree).unwrap()).unwrap_err();
        assert!(
            error.to_string().contains("Preprocessor.mlmodelc/"),
            "{error}"
        );
    }

    #[test]
    fn paths_that_leave_the_folder_are_rejected() {
        let tree = r#"[{"type":"file","oid":"b","size":2,"path":"nemotron_coreml_560ms/../../evil.json"}]"#;
        assert!(manifest(&LIVE_ASR, parse_tree(tree).unwrap()).is_err());
        assert!(is_safe_relative("a/b.mlmodelc/weights/weight.bin"));
        for path in ["", "/etc/passwd", "a/../b", "./a", "a\\b"] {
            assert!(!is_safe_relative(path), "{path}");
        }
    }

    #[test]
    fn a_bad_checksum_is_rejected() {
        let tree = r#"[{"type":"file","oid":"b","size":9,"lfs":{"oid":"xyz","size":9},
            "path":"silero-vad-unified-256ms-v6.2.1.mlmodelc/weights/weight.bin"}]"#;
        assert!(manifest(&VAD, parse_tree(tree).unwrap()).is_err());
    }

    #[test]
    fn the_next_page_comes_from_the_link_header() {
        assert_eq!(
            next_page(Some(
                "<https://huggingface.co/api/models/x/tree/main?cursor=abc>; rel=\"next\""
            ))
            .as_deref(),
            Some("https://huggingface.co/api/models/x/tree/main?cursor=abc")
        );
        assert_eq!(next_page(Some("<https://a>; rel=\"prev\"")), None);
        assert_eq!(next_page(None), None);
    }

    #[test]
    fn bundles_are_pinned_and_named_after_their_repo() {
        for kind in ModelKind::ALL {
            let bundle = bundle(kind);
            assert_eq!(bundle.kind, kind);
            assert_eq!(bundle.id, bundle.dir_name);
            assert_eq!(bundle.repo.rsplit('/').next(), Some(bundle.dir_name));
            assert_eq!(bundle.revision.len(), 40);
            assert!(bundle.revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(bundle.approx_bytes > 0);
            assert!(!bundle.include.is_empty());
            let url = bundle.file_url("config.json");
            assert!(
                url.contains(&format!("/resolve/{}/", bundle.revision)),
                "{url}"
            );
            assert!(bundle.tree_url().contains(bundle.revision));
        }
        let ids: Vec<&str> = BUNDLES.iter().map(|bundle| bundle.id).collect();
        assert_eq!(
            ids,
            vec![
                "nemotron-speech-streaming-en-0.6b-coreml",
                "parakeet-tdt-0.6b-v2-coreml",
                "silero-vad-coreml",
                "speaker-diarization-coreml",
            ]
        );
        assert_eq!(
            LIVE_ASR.file_url("nemotron_coreml_560ms/encoder/encoder_int8.mlmodelc/weights/weight.bin"),
            "https://huggingface.co/FluidInference/nemotron-speech-streaming-en-0.6b-coreml/resolve/e673531caa6d25ab7baf5a8c14c9b99ba1551838/nemotron_coreml_560ms/encoder/encoder_int8.mlmodelc/weights/weight.bin"
        );
        assert_eq!(encode_path("a b/c#d"), "a%20b/c%23d");
    }

    #[test]
    fn a_bundle_is_downloaded_only_with_a_marker_for_its_revision() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join(VAD.dir_name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("config.json"), "{}").unwrap();
        assert!(!VAD.is_downloaded_in(root.path()));
        std::fs::write(
            folder.join(COMPLETE_MARKER),
            "0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        assert!(!VAD.is_downloaded_in(root.path()));
        std::fs::write(folder.join(COMPLETE_MARKER), format!("{}\n", VAD.revision)).unwrap();
        assert!(VAD.is_downloaded_in(root.path()));
        assert!(!FINAL_ASR.is_downloaded_in(root.path()));
    }

    #[test]
    fn overall_progress_counts_finished_files_and_the_one_in_flight() {
        assert_eq!(overall_bytes(0, 200, 50), 50);
        assert_eq!(overall_bytes(1_000, 200, 200), 1_200);
        assert_eq!(overall_bytes(1_000, 200, 500), 1_200);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(300, 200), 100);
        assert_eq!(percent(0, 0), 0);
    }

    #[test]
    fn downloads_run_in_order_and_skip_what_is_on_disk() {
        let all = ModelKind::ALL;
        assert_eq!(
            missing_in_order(
                &[ModelKind::LiveAsr, ModelKind::Vad, ModelKind::FinalAsr],
                |_| false
            ),
            vec![ModelKind::FinalAsr, ModelKind::Vad, ModelKind::LiveAsr]
        );
        assert_eq!(
            missing_in_order(&all, |kind| kind == ModelKind::Vad),
            vec![
                ModelKind::FinalAsr,
                ModelKind::Diarization,
                ModelKind::LiveAsr
            ]
        );
        assert!(missing_in_order(&all, |_| true).is_empty());
    }

    #[test]
    fn progress_events_are_camel_case_with_kebab_case_kinds() {
        let event = terminal_event(&DIARIZATION, "completed", None);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["modelId"], "speaker-diarization-coreml");
        assert_eq!(json["kind"], "diarization");
        assert_eq!(json["progress"], 100);
        assert_eq!(json["status"], "completed");
        assert!(json.get("downloadedMb").is_some());
        assert!(json.get("totalMb").is_some());
        assert!(json.get("speedMbps").is_some());
        assert!(json["etaSeconds"].is_null());
        assert!(json["error"].is_null());
        assert_eq!(
            serde_json::to_value(ModelKind::LiveAsr).unwrap(),
            "live-asr"
        );
        assert_eq!(
            serde_json::from_value::<ModelKind>(serde_json::json!("final-asr")).unwrap(),
            ModelKind::FinalAsr
        );
    }

    #[test]
    fn pruning_keeps_only_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("A.mlmodelc/weights/weight.bin");
        let stale = dir.path().join("Old.mlmodelc/model.mil");
        for path in [&kept, &stale] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        prune(dir.path(), &HashSet::from([kept.clone()])).unwrap();
        assert!(kept.exists());
        assert!(!stale.exists());
        assert!(!dir.path().join("Old.mlmodelc").exists());
    }

    #[test]
    fn missing_models_are_coming_unless_the_os_or_the_user_rules_them_out() {
        use Availability::*;
        assert_eq!(availability_of(true, true, false, false), Ready);
        assert_eq!(availability_of(true, false, true, true), Coming);
        assert_eq!(availability_of(true, false, false, false), Coming);
        assert_eq!(availability_of(true, false, false, true), Unavailable);
        // Below macOS 15 nothing runs, downloaded or not.
        assert_eq!(availability_of(false, true, false, false), Unavailable);
        assert_eq!(availability_of(false, false, true, false), Unavailable);
    }

    fn touch(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn only_the_removed_engines_models_are_retired() {
        let models = tempfile::tempdir().unwrap();
        let root = models.path();
        let retired = [
            "ggml-large-v3-turbo-q5_0.bin",
            "ggml-large-v3-turbo-encoder.mlmodelc/weights/weight.bin",
            "parakeet/parakeet-tdt-0.6b-v3-int8/encoder-model.int8.onnx",
            "nemotron-streaming/encoder.int8.onnx",
            "diarization/nemotron-3/model.onnx",
            "diarization/wespeaker_en_voxceleb_resnet34_LM.onnx",
        ];
        let kept = [
            "summary/Qwen3.5-2B-Q4_K_M.gguf",
            "fluidaudio/silero-vad-coreml/config.json",
            "ggml-custom.bin",
            "notes.txt",
        ];
        for path in retired.iter().chain(&kept) {
            touch(&root.join(path), 10);
        }
        touch(&root.join("nemotron-streaming-coreml/metadata.json"), 4);

        // While the new live model is missing, the old one stays for reuse.
        let (removed, complete) = remove_retired(root, true);
        assert!(!complete);
        assert_eq!(removed.len(), 5);
        assert_eq!(removed.iter().map(|(_, bytes)| bytes).sum::<u64>(), 60);
        for path in retired {
            assert!(!root.join(path).exists(), "{path}");
        }
        for path in kept {
            assert!(root.join(path).exists(), "{path}");
        }
        assert!(root.join("nemotron-streaming-coreml").exists());

        let (removed, complete) = remove_retired(root, false);
        assert!(complete);
        assert_eq!(removed, vec![(root.join("nemotron-streaming-coreml"), 4)]);
        assert!(retired_paths(root, false).is_empty());
        assert!(root.join("fluidaudio").exists() && root.join("summary").exists());
    }

    #[test]
    fn the_old_live_model_moves_into_the_new_layout() {
        let models = tempfile::tempdir().unwrap();
        let old = models.path().join(RETIRED_LIVE_COREML_DIR);
        let folder = models.path().join("fluidaudio").join(LIVE_ASR.dir_name);
        let weights = "encoder/encoder_int8.mlmodelc/weights/weight.bin";
        touch(&old.join(weights), 8);
        touch(&old.join("metadata.json"), 3);
        touch(&old.join("stray.txt"), 1);
        // Already in place: left alone.
        touch(&folder.join("nemotron_coreml_560ms/metadata.json"), 5);
        let file = |path: &str, bytes| ManifestFile {
            path: path.into(),
            bytes,
            sha256: None,
        };
        let files = [
            file("config.json", 2),
            file("nemotron_coreml_560ms/metadata.json", 3),
            file(&format!("nemotron_coreml_560ms/{weights}"), 8),
        ];
        adopt_retired_live_files(&old, &folder, &files);
        assert_eq!(
            std::fs::metadata(folder.join("nemotron_coreml_560ms").join(weights))
                .unwrap()
                .len(),
            8
        );
        assert!(!old.join(weights).exists());
        assert_eq!(
            std::fs::metadata(folder.join("nemotron_coreml_560ms/metadata.json"))
                .unwrap()
                .len(),
            5
        );
        assert!(old.join("metadata.json").exists());
        assert!(old.join("stray.txt").exists());
        assert!(!folder.join("stray.txt").exists());
    }

    #[tokio::test]
    async fn a_file_is_reused_only_with_its_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weight.bin");
        std::fs::write(&path, b"hello").unwrap();
        let lfs = |sha256: &str| ManifestFile {
            path: "weight.bin".into(),
            bytes: 5,
            sha256: Some(sha256.into()),
        };
        // sha256("hello")
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(reusable(&path, &lfs(good)).await);
        assert!(!reusable(&path, &lfs(&"0".repeat(64))).await);
        let git = ManifestFile {
            path: "weight.bin".into(),
            bytes: 5,
            sha256: None,
        };
        assert!(!reusable(&path, &git).await);
    }
}
