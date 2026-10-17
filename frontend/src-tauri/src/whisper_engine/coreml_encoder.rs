//! Core ML encoders for Whisper: whisper.cpp runs a model's encoder (most of
//! its work) on the Apple Neural Engine when
//! `ggml-<model>-encoder.mlmodelc` sits next to the ggml file. The app is
//! built with whisper-rs's `coreml` feature (with fallback), so without the
//! folder everything stays on the Metal GPU, as before.
//!
//! The encoders come from whisper.cpp's own Hugging Face repository, pinned to
//! one revision, and every zip is checked against its size and SHA-256 before
//! it is unpacked. A folder only gets its final name once fully unpacked, so a
//! half-written encoder is never loaded.
//!
//! The first load of an encoder compiles it for the Neural Engine, which takes
//! a while; [`spawn_setup`] does that once in the background after the
//! download, never at the end of a meeting.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use log::{info, warn};
use tauri::{AppHandle, Manager, Runtime};

const REPO: &str = "ggerganov/whisper.cpp";
/// Pinned revision of [`REPO`].
pub const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

pub struct EncoderFile {
    /// The model's base name: `large-v3-turbo` for `large-v3-turbo-q5_0`.
    pub base: &'static str,
    pub zip_bytes: u64,
    pub zip_sha256: &'static str,
}

/// The encoders the app installs: the large models used for the final pass.
pub const ENCODERS: [EncoderFile; 2] = [
    EncoderFile {
        base: "large-v3-turbo",
        zip_bytes: 1_173_393_014,
        zip_sha256: "84bedfe895bd7b5de6e8e89a0803dfc5addf8c0c5bc4c937451716bf7cf7988a",
    },
    EncoderFile {
        base: "large-v3",
        zip_bytes: 1_175_711_232,
        zip_sha256: "47837be7594a29429ec08620043390c4d6d467f8bd362df09e9390ace76a55a4",
    },
];

/// The base name whisper.cpp looks up the encoder by: the model name without
/// a `-qX_Y` quantization suffix (the same rule as
/// `whisper_get_coreml_path_encoder`).
pub fn encoder_base(model_name: &str) -> &str {
    if let Some(pos) = model_name.rfind('-') {
        let suffix = model_name[pos..].as_bytes();
        if suffix.len() == 5 && suffix[1] == b'q' && suffix[3] == b'_' {
            return &model_name[..pos];
        }
    }
    model_name
}

/// The encoder for a Whisper model, when the app offers one.
pub fn encoder_for(model_name: &str) -> Option<&'static EncoderFile> {
    let base = encoder_base(model_name);
    ENCODERS.iter().find(|encoder| encoder.base == base)
}

fn folder_name(base: &str) -> String {
    format!("ggml-{base}-encoder.mlmodelc")
}

pub fn encoder_dir(models_dir: &Path, model_name: &str) -> PathBuf {
    models_dir.join(folder_name(encoder_base(model_name)))
}

pub fn zip_url(encoder: &EncoderFile) -> String {
    format!(
        "https://huggingface.co/{REPO}/resolve/{REVISION}/{}.zip",
        folder_name(encoder.base)
    )
}

/// Whether the encoder is unpacked and complete. The folder is only renamed
/// into place after a full unpack, so its weights being there is enough.
pub fn is_installed(models_dir: &Path, model_name: &str) -> bool {
    let dir = encoder_dir(models_dir, model_name);
    dir.join("model.mil").is_file()
        && dir
            .join("weights")
            .join("weight.bin")
            .metadata()
            .is_ok_and(|m| m.len() > 0)
}

/// Where an archive entry goes inside the encoder folder, or `None` to skip
/// it. Only plain relative paths under the encoder's own top folder are
/// accepted; macOS resource forks (`__MACOSX/`) are skipped.
pub fn entry_target(entry_name: &str, top: &str) -> Result<Option<PathBuf>> {
    let path = Path::new(entry_name);
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == "__MACOSX" => return Ok(None),
        Some(Component::Normal(first)) if first == top => {}
        _ => bail!("Unexpected entry in the Core ML encoder archive: {entry_name}"),
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            _ => bail!("Unsafe path in the Core ML encoder archive: {entry_name}"),
        }
    }
    Ok(Some(relative))
}

/// Unpack `zip_path` into `destination` (the final encoder folder) through a
/// temporary folder next to it, renamed into place at the end.
pub fn unpack(zip_path: &Path, destination: &Path) -> Result<()> {
    let top = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("Bad encoder folder {}", destination.display()))?
        .to_string();
    let staging = destination.with_extension("mlmodelc.unpacking");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<()> {
        let file = std::fs::File::open(zip_path)?;
        let mut archive = zip::ZipArchive::new(file).context("Not a valid zip archive")?;
        let mut files = 0usize;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_symlink() {
                bail!("Symlink in the Core ML encoder archive: {}", entry.name());
            }
            let Some(relative) = entry_target(entry.name(), &top)? else {
                continue;
            };
            let target = staging.join(&relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&target)?;
            let mut buffer = vec![0u8; 1 << 20];
            loop {
                let read = entry.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, &buffer[..read])?;
            }
            files += 1;
        }
        if files == 0 || !staging.join("model.mil").is_file() {
            bail!("The Core ML encoder archive did not contain an encoder");
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    std::fs::rename(&staging, destination)?;
    Ok(())
}

static SETTING_UP: AtomicBool = AtomicBool::new(false);

/// Download, verify and unpack the encoder for `model_name` into `models_dir`.
pub async fn install(models_dir: &Path, model_name: &str) -> Result<()> {
    let encoder =
        encoder_for(model_name).ok_or_else(|| anyhow!("No Core ML encoder for {model_name}"))?;
    let destination = encoder_dir(models_dir, model_name);
    let zip_path = models_dir.join(format!("{}.zip", folder_name(encoder.base)));
    let client = reqwest::Client::builder()
        .connect_timeout(crate::audio::diarization::DOWNLOAD_CONNECT_TIMEOUT)
        .build()?;
    info!(
        "Core ML encoder: downloading {} ({} MB)",
        folder_name(encoder.base),
        encoder.zip_bytes / 1_000_000
    );
    let started = Instant::now();
    crate::audio::diarization::download_verified(
        &client,
        &zip_url(encoder),
        &zip_path,
        encoder.zip_bytes,
        encoder.zip_sha256,
        |_| {},
    )
    .await
    .context("Core ML encoder download failed")?;
    let zip_for_unpack = zip_path.clone();
    let unpacked =
        crate::quiet_work::spawn_quietly(move || unpack(&zip_for_unpack, &destination)).await;
    let _ = tokio::fs::remove_file(&zip_path).await;
    unpacked.map_err(|e| anyhow!("Unpack task failed: {e}"))??;
    info!(
        "Core ML encoder for {model_name} installed in {:.0?}",
        started.elapsed()
    );
    Ok(())
}

/// Compile the encoder for the Neural Engine by loading it once, so the first
/// final pass does not pay for it. Core ML caches the compiled model.
fn precompile(models_dir: &Path, model_name: &str) -> Result<Duration> {
    let model_path = models_dir.join(format!("ggml-{model_name}.bin"));
    if !model_path.is_file() {
        bail!("{} is not downloaded", model_path.display());
    }
    crate::quiet_work::run_quietly(|| {
        let started = Instant::now();
        // CPU-only context: the point is the encoder's compile, not Metal.
        let params = whisper_rs::WhisperContextParameters {
            use_gpu: false,
            ..Default::default()
        };
        let ctx =
            whisper_rs::WhisperContext::new_with_params(&model_path.to_string_lossy(), params)
                .map_err(|e| anyhow!("Could not load {model_name}: {e}"))?;
        let _state = ctx
            .create_state()
            .map_err(|e| anyhow!("Could not load the Core ML encoder: {e}"))?;
        Ok(started.elapsed())
    })
}

/// In the background after startup: install the Core ML encoder for the final
/// model when it is downloaded but its encoder is not, then compile it once.
/// Never during a recording; a failure is retried on a later start.
pub fn spawn_setup<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(90)).await;
        if SETTING_UP.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(error) = setup(&app).await {
            warn!("Core ML encoder setup did not finish, retrying on a later start: {error:#}");
        }
        SETTING_UP.store(false, Ordering::SeqCst);
    });
}

async fn setup<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let engine = crate::audio::transcription_settings::configured_final_model(app).await;
    if engine.is_parakeet() || encoder_for(&engine.model).is_none() {
        return Ok(());
    }
    let models_dir = app.path().app_data_dir()?.join("models");
    if !models_dir
        .join(format!("ggml-{}.bin", engine.model))
        .is_file()
    {
        return Ok(());
    }
    if !is_installed(&models_dir, &engine.model) {
        install(&models_dir, &engine.model).await?;
    } else {
        // Installed earlier: compiled then too.
        return Ok(());
    }
    // Compile when nothing is recording or transcribing.
    while crate::audio::recording_commands::is_recording().await
        || crate::audio::retranscription::is_retranscription_in_progress()
    {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    let model = engine.model.clone();
    let took = tokio::task::spawn_blocking(move || precompile(&models_dir, &model))
        .await
        .map_err(|e| anyhow!("Compile task failed: {e}"))??;
    info!(
        "Core ML encoder for {} compiled in {:.1?}",
        engine.model, took
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_encoder_name_drops_the_quantization_like_whisper_cpp() {
        assert_eq!(encoder_base("large-v3-turbo-q5_0"), "large-v3-turbo");
        assert_eq!(encoder_base("large-v3-q5_0"), "large-v3");
        assert_eq!(encoder_base("large-v3-turbo"), "large-v3-turbo");
        assert_eq!(encoder_base("small-q5_1"), "small");
        assert_eq!(encoder_base("base"), "base");
        assert!(encoder_for("large-v3-turbo-q5_0").is_some());
        assert!(encoder_for("large-v3-turbo-q8_0").is_some());
        assert!(encoder_for("small").is_none());
        assert_eq!(
            encoder_dir(Path::new("/m"), "large-v3-turbo-q5_0"),
            PathBuf::from("/m/ggml-large-v3-turbo-encoder.mlmodelc")
        );
    }

    #[test]
    fn downloads_are_pinned_and_hashed() {
        for encoder in ENCODERS.iter() {
            let url = zip_url(encoder);
            assert!(url.contains(REVISION), "{url}");
            assert!(!url.contains("/main/"));
            assert!(url.ends_with("-encoder.mlmodelc.zip"));
            assert_eq!(encoder.zip_sha256.len(), 64);
            assert!(encoder.zip_bytes > 1_000_000_000);
        }
    }

    #[test]
    fn archive_entries_must_stay_inside_the_encoder_folder() {
        let top = "ggml-x-encoder.mlmodelc";
        assert_eq!(
            entry_target("ggml-x-encoder.mlmodelc/weights/weight.bin", top).unwrap(),
            Some(PathBuf::from("weights/weight.bin"))
        );
        assert_eq!(
            entry_target("__MACOSX/ggml-x-encoder.mlmodelc/._model.mil", top).unwrap(),
            None
        );
        assert!(entry_target("ggml-x-encoder.mlmodelc/../evil", top).is_err());
        assert!(entry_target("/etc/passwd", top).is_err());
        assert!(entry_target("other/model.mil", top).is_err());
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, options).unwrap();
            } else {
                zip.start_file(*name, options).unwrap();
                zip.write_all(body).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    #[test]
    fn unpacking_installs_the_folder_and_skips_resource_forks() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("enc.zip");
        write_zip(
            &zip_path,
            &[
                ("ggml-large-v3-turbo-encoder.mlmodelc/", b""),
                ("ggml-large-v3-turbo-encoder.mlmodelc/model.mil", b"mil"),
                (
                    "ggml-large-v3-turbo-encoder.mlmodelc/weights/weight.bin",
                    b"weights",
                ),
                (
                    "__MACOSX/ggml-large-v3-turbo-encoder.mlmodelc/._model.mil",
                    b"x",
                ),
            ],
        );
        let destination = encoder_dir(dir.path(), "large-v3-turbo-q5_0");
        assert!(!is_installed(dir.path(), "large-v3-turbo-q5_0"));
        unpack(&zip_path, &destination).unwrap();
        assert!(is_installed(dir.path(), "large-v3-turbo-q5_0"));
        assert!(!dir.path().join("__MACOSX").exists());
        assert!(!destination.with_extension("mlmodelc.unpacking").exists());
    }

    #[test]
    fn a_bad_archive_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("enc.zip");
        write_zip(
            &zip_path,
            &[
                ("ggml-large-v3-turbo-encoder.mlmodelc/model.mil", b"mil"),
                ("ggml-large-v3-turbo-encoder.mlmodelc/../../escape", b"x"),
            ],
        );
        let destination = encoder_dir(dir.path(), "large-v3-turbo-q5_0");
        assert!(unpack(&zip_path, &destination).is_err());
        assert!(!destination.exists());
        assert!(!destination.with_extension("mlmodelc.unpacking").exists());

        std::fs::write(&zip_path, b"not a zip").unwrap();
        assert!(unpack(&zip_path, &destination).is_err());
        assert!(!destination.exists());
    }
}
