//! LoRA adapters for the fine-tuned live notes model (Gemma 4 E2B): one for
//! the live notes windows, one for the end-of-meeting tidy pass. They are
//! GGUF files (llama.cpp's LoRA format) next to the summary models, applied
//! per request by llama-helper (`GenerationOptions::lora_path`).

use std::path::{Path, PathBuf};

/// Which fine-tuned adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    LiveNotes,
    Tidy,
}

impl Adapter {
    /// The environment variable that points at a development copy.
    fn env_var(self) -> &'static str {
        match self {
            Adapter::LiveNotes => "MEETILY_NOTES_LORA",
            Adapter::Tidy => "MEETILY_TIDY_LORA",
        }
    }

    /// The file name in the models folder.
    fn file_name(self) -> &'static str {
        match self {
            // v3: stage 3, trained without action items (its prompt is
            // `live_summary::finetuned::SYSTEM_PROMPT`).
            Adapter::LiveNotes => "minutes-notes-lora-v3.gguf",
            Adapter::Tidy => "minutes-tidy-lora.gguf",
        }
    }

    /// Where the app downloads it, and the SHA-256 the file must have.
    fn download(self) -> (&'static str, &'static str) {
        match self {
            Adapter::LiveNotes => (
                "https://huggingface.co/praveenvnktsh00/minutes-notes-gemma4-e2b/resolve/main/minutes-notes-lora-v3.gguf",
                "5f7d625017112df3c18df7bc1bd5a5ffe3b017946345c59aa6337eba8754d2c9",
            ),
            Adapter::Tidy => (
                "https://huggingface.co/praveenvnktsh00/minutes-tidy-gemma4-e2b/resolve/main/minutes-tidy-lora.gguf",
                "1d96c706130435f2f29591e3b777acade419630f3b0e07d2f26b1fd1d4bb703d",
            ),
        }
    }
}

/// Adapter files earlier versions used.
const RETIRED: [&str; 1] = ["minutes-notes-lora.gguf"];

/// Downloads whichever adapter is missing from `models_dir` (52 MB each),
/// checking its SHA-256. Run once the live notes model is on disk: after its
/// download and at startup. Failures are logged; the base model runs until a
/// later attempt succeeds.
pub async fn ensure_downloaded(models_dir: &Path) {
    // Adapters replaced by a newer version: each was trained with another
    // prompt, so it is only taking space.
    for old in RETIRED {
        let _ = tokio::fs::remove_file(models_dir.join(old)).await;
    }
    for which in [Adapter::LiveNotes, Adapter::Tidy] {
        let path = models_dir.join(which.file_name());
        if path.is_file() {
            continue;
        }
        let (url, sha256) = which.download();
        match download(url, sha256, &path).await {
            Ok(()) => log::info!("Downloaded the {which:?} adapter to {}", path.display()),
            Err(error) => log::warn!("Could not download the {which:?} adapter: {error}"),
        }
    }
}

async fn download(url: &str, sha256: &str, path: &Path) -> anyhow::Result<()> {
    let partial = path.with_extension("gguf.partial");
    let bytes = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    tokio::fs::write(&partial, &bytes).await?;
    let check = partial.clone();
    let actual = tokio::task::spawn_blocking(move || super::model_manager::sha256_file(&check)).await??;
    if !actual.eq_ignore_ascii_case(sha256) {
        let _ = tokio::fs::remove_file(&partial).await;
        anyhow::bail!("checksum mismatch (got {actual})");
    }
    tokio::fs::rename(&partial, path).await?;
    Ok(())
}

/// The adapter's file: the environment override when it names a file that
/// exists, else `<models_dir>/<file name>` when that exists, else `None`
/// (run the base model). `models_dir` is the summary models folder,
/// `models::get_models_directory(app_data_dir)`.
pub fn adapter_path(models_dir: &Path, which: Adapter) -> Option<PathBuf> {
    resolve(
        std::env::var_os(which.env_var()).map(PathBuf::from),
        models_dir,
        which,
    )
}

/// [`adapter_path`] with the environment value passed in.
fn resolve(env_override: Option<PathBuf>, models_dir: &Path, which: Adapter) -> Option<PathBuf> {
    if let Some(path) = env_override.filter(|path| path.is_file()) {
        return Some(path);
    }
    Some(models_dir.join(which.file_name())).filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_wins_then_the_models_folder_then_none() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        assert_eq!(resolve(None, &models, Adapter::LiveNotes), None);

        let bundled = models.join("minutes-notes-lora-v3.gguf");
        std::fs::write(&bundled, b"gguf").unwrap();
        assert_eq!(
            resolve(None, &models, Adapter::LiveNotes),
            Some(bundled.clone())
        );
        assert_eq!(resolve(None, &models, Adapter::Tidy), None);

        // An override that does not exist is ignored.
        let missing = dir.path().join("missing.gguf");
        assert_eq!(
            resolve(Some(missing), &models, Adapter::LiveNotes),
            Some(bundled)
        );
        let dev = dir.path().join("dev-tidy.gguf");
        std::fs::write(&dev, b"gguf").unwrap();
        assert_eq!(
            resolve(Some(dev.clone()), &models, Adapter::Tidy),
            Some(dev)
        );
    }
}
