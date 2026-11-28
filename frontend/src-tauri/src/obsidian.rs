//! Save a meeting's Markdown export into an Obsidian vault.
//!
//! Saving is manual and never overwrites: a second save of the same meeting
//! (or another meeting with the same title and date) lands next to the first
//! as `<name> v2.md`, `v3`, … so edits made in Obsidian are never touched.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use tauri::{AppHandle, Runtime};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_store::StoreExt;

use crate::api::sanitize_export_file_name;

const STORE_FILE: &str = "obsidian-export.json";
const STORE_KEY: &str = "config";
const DEFAULT_TAG: &str = "minutes-app";
/// Far beyond any real number of re-saves; stops a runaway loop if the
/// directory misbehaves (every create reporting `AlreadyExists`).
const MAX_VERSIONS: u32 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObsidianExportConfig {
    /// Absolute path of the vault's root folder.
    pub vault_path: Option<String>,
    /// Folder inside the vault, relative, `/`-separated; empty for the root.
    pub folder: String,
    pub tags: Vec<String>,
    pub include_transcript: bool,
}

impl Default for ObsidianExportConfig {
    fn default() -> Self {
        Self {
            vault_path: None,
            folder: String::new(),
            tags: vec![DEFAULT_TAG.to_string()],
            include_transcript: true,
        }
    }
}

/// The stored settings plus anything worth telling the user about them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObsidianExportState {
    pub config: ObsidianExportConfig,
    /// Set when the vault folder has no `.obsidian/` (it may not be a vault).
    /// Saving still works.
    pub vault_warning: Option<String>,
    /// Set when the stored vault can't be used (moved or deleted). Saving
    /// fails until the user picks it again.
    pub vault_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedObsidianNote {
    /// Absolute path of the new file.
    pub path: String,
    /// Path inside the vault, `/`-separated, as Obsidian names it.
    pub relative_path: String,
    pub vault_name: String,
    /// 1 for the plain name, 2 for ` v2`, …
    pub version: u32,
}

/// Tags as Obsidian accepts them: no leading `#`, spaces as `-`, only
/// letters, digits, `_`, `/` and `-`, not purely numeric, each once
/// (case-insensitively, as Obsidian matches tags).
pub fn normalise_tags<S: AsRef<str>>(tags: &[S]) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.iter()
        .filter_map(|tag| {
            let tag = tag.as_ref().trim().trim_start_matches('#');
            let tag: String = tag
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '/' | '-'))
                .collect();
            let tag = tag.trim_matches('/').to_string();
            if tag.is_empty() || tag.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            seen.insert(tag.to_lowercase()).then_some(tag)
        })
        .collect()
}

/// A folder inside the vault, normalised to `a/b` form. Absolute paths and
/// `..` are refused so the folder can never point outside the vault.
fn normalise_folder(folder: &str) -> Result<String, String> {
    let folder = folder.trim();
    let mut parts = Vec::new();
    for component in Path::new(folder).components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| "The folder name is not valid text".to_string())?
                    .to_string(),
            ),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("The folder must be inside the vault (no \"..\")".to_string())
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("The folder must be relative to the vault".to_string())
            }
        }
    }
    Ok(parts.join("/"))
}

/// Checks the vault is an existing folder; warns (without failing) when it
/// has no `.obsidian/` and so may not be a vault Obsidian knows.
fn check_vault(vault: &str) -> Result<(PathBuf, Option<String>), String> {
    let path = PathBuf::from(vault.trim());
    if !path.is_absolute() {
        return Err("The vault path must be absolute".to_string());
    }
    if !path.is_dir() {
        return Err(format!(
            "The vault folder {} does not exist",
            path.display()
        ));
    }
    let warning = (!path.join(".obsidian").is_dir()).then(|| {
        "This folder has no .obsidian folder, so it may not be an Obsidian vault. \
         Notes will still be saved here."
            .to_string()
    });
    Ok((path, warning))
}

/// `dir` must resolve (following symlinks) to the vault or a folder inside it.
fn ensure_inside(vault: &Path, dir: &Path) -> Result<(), String> {
    let vault = vault
        .canonicalize()
        .map_err(|e| format!("Could not resolve the vault folder: {e}"))?;
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("Could not resolve the export folder: {e}"))?;
    if dir.starts_with(&vault) {
        Ok(())
    } else {
        Err("The folder must be inside the vault".to_string())
    }
}

/// The config as it will be stored, or why it can't be.
fn validate_config(config: ObsidianExportConfig) -> Result<ObsidianExportState, String> {
    let folder = normalise_folder(&config.folder)?;
    let tags = normalise_tags(&config.tags);
    let vault_path = config
        .vault_path
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty());

    let mut vault_warning = None;
    if let Some(vault) = &vault_path {
        let (vault_dir, warning) = check_vault(vault)?;
        vault_warning = warning;
        // A folder that exists already must not be a symlink leading out of
        // the vault; one that doesn't exist yet is created on first save.
        let target = vault_dir.join(&folder);
        if target.exists() {
            ensure_inside(&vault_dir, &target)?;
        }
    }

    Ok(ObsidianExportState {
        config: ObsidianExportConfig {
            vault_path,
            folder,
            tags,
            include_transcript: config.include_transcript,
        },
        vault_warning,
        vault_error: None,
    })
}

/// `<stem>.md` for version 1, `<stem> vN.md` after.
fn versioned_name(file_name: &str, version: u32) -> String {
    if version <= 1 {
        return file_name.to_string();
    }
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} v{version}.{ext}"),
        _ => format!("{file_name} v{version}"),
    }
}

/// Writes `contents` into `dir` under the sanitised `file_name`, or the first
/// free ` vN` version of it. Each attempt creates the file exclusively, so an
/// existing note is never opened for writing, even by two saves at once.
fn write_new_note(dir: &Path, file_name: &str, contents: &str) -> Result<(PathBuf, u32), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    // Obsidian hides dot-files, so a leading `.` would make the note vanish.
    let mut base = sanitize_export_file_name(file_name)
        .trim_start_matches('.')
        .to_string();
    if base.is_empty() {
        base = "meeting-export".to_string();
    }
    if !base.to_lowercase().ends_with(".md") {
        base.push_str(".md");
    }

    for version in 1..=MAX_VERSIONS {
        let path = dir.join(versioned_name(&base, version));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file
                    .write_all(contents.as_bytes())
                    .and_then(|_| file.sync_all())
                {
                    // Don't leave a truncated note behind; it's ours, just created.
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("Could not write {}: {error}", path.display()));
                }
                return Ok((path, version));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create {}: {error}", path.display())),
        }
    }
    Err("Too many saved versions of this note".to_string())
}

/// Percent-encodes everything but RFC 3986 unreserved characters, like
/// `encodeURIComponent` (so `/` and spaces are encoded too).
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn obsidian_open_url(vault_name: &str, relative_path: &str) -> String {
    format!(
        "obsidian://open?vault={}&file={}",
        encode_component(vault_name),
        encode_component(relative_path)
    )
}

fn load_config<R: Runtime>(app: &AppHandle<R>) -> Result<ObsidianExportConfig, String> {
    let store = app
        .store(STORE_FILE)
        .map_err(|e| format!("Failed to open Obsidian export settings: {e}"))?;
    match store.get(STORE_KEY) {
        Some(value) => serde_json::from_value(value)
            .map_err(|e| format!("Failed to read Obsidian export settings: {e}")),
        None => Ok(ObsidianExportConfig::default()),
    }
}

#[tauri::command]
pub async fn get_obsidian_export_config<R: Runtime>(
    app: AppHandle<R>,
) -> Result<ObsidianExportState, String> {
    let config = load_config(&app)?;
    // A vault that has since moved still loads, so the user can fix it.
    let (vault_warning, vault_error) = match config.vault_path.as_deref().map(check_vault) {
        Some(Ok((_, warning))) => (warning, None),
        Some(Err(error)) => (None, Some(error)),
        None => (None, None),
    };
    Ok(ObsidianExportState {
        config,
        vault_warning,
        vault_error,
    })
}

#[tauri::command]
pub async fn set_obsidian_export_config<R: Runtime>(
    app: AppHandle<R>,
    config: ObsidianExportConfig,
) -> Result<ObsidianExportState, String> {
    let state = validate_config(config)?;
    let store = app
        .store(STORE_FILE)
        .map_err(|e| format!("Failed to open Obsidian export settings: {e}"))?;
    store.set(
        STORE_KEY,
        serde_json::to_value(&state.config)
            .map_err(|e| format!("Failed to serialize Obsidian export settings: {e}"))?,
    );
    store
        .save()
        .map_err(|e| format!("Failed to save Obsidian export settings: {e}"))?;
    Ok(state)
}

async fn pick_folder<R: Runtime>(
    app: &AppHandle<R>,
    start: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let app = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        let mut dialog = app.dialog().file();
        if let Some(start) = start {
            dialog = dialog.set_directory(start);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(|e| format!("Folder dialog failed: {e}"))?;
    picked
        .map(|path| {
            path.into_path()
                .map_err(|e| format!("Unsupported folder: {e}"))
        })
        .transpose()
}

#[tauri::command]
pub async fn pick_obsidian_vault<R: Runtime>(app: AppHandle<R>) -> Result<Option<String>, String> {
    Ok(pick_folder(&app, None)
        .await?
        .map(|path| path.to_string_lossy().to_string()))
}

/// Picks a folder starting at the vault and returns it relative to the vault.
#[tauri::command]
pub async fn pick_obsidian_folder<R: Runtime>(
    app: AppHandle<R>,
    vault_path: String,
) -> Result<Option<String>, String> {
    let (vault, _) = check_vault(&vault_path)?;
    let Some(picked) = pick_folder(&app, Some(vault.clone())).await? else {
        return Ok(None);
    };
    relative_to_vault(&vault, &picked).map(Some)
}

fn relative_to_vault(vault: &Path, picked: &Path) -> Result<String, String> {
    ensure_inside(vault, picked)?;
    let vault = vault.canonicalize().map_err(|e| e.to_string())?;
    let picked = picked.canonicalize().map_err(|e| e.to_string())?;
    let relative = picked
        .strip_prefix(&vault)
        .map_err(|_| "The folder must be inside the vault".to_string())?;
    normalise_folder(&relative.to_string_lossy())
}

#[tauri::command]
pub async fn save_obsidian_note<R: Runtime>(
    app: AppHandle<R>,
    file_name: String,
    contents: String,
) -> Result<SavedObsidianNote, String> {
    let state = validate_config(load_config(&app)?)?;
    let vault = state
        .config
        .vault_path
        .ok_or_else(|| "Choose an Obsidian vault first".to_string())?;
    save_into_vault(
        Path::new(&vault),
        &state.config.folder,
        &file_name,
        &contents,
    )
}

fn save_into_vault(
    vault: &Path,
    folder: &str,
    file_name: &str,
    contents: &str,
) -> Result<SavedObsidianNote, String> {
    let dir = if folder.is_empty() {
        vault.to_path_buf()
    } else {
        vault.join(folder)
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    ensure_inside(vault, &dir)?;

    let (path, version) = write_new_note(&dir, file_name, contents)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let relative_path = if folder.is_empty() {
        name
    } else {
        format!("{folder}/{name}")
    };
    let vault_name = vault
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    log::info!("Saved Obsidian note to {}", path.display());
    Ok(SavedObsidianNote {
        path: path.to_string_lossy().to_string(),
        relative_path,
        vault_name,
        version,
    })
}

/// Opens a note in Obsidian. Separate from `open_external_url`, whose
/// allowlist is web-only, and able to open nothing but an `obsidian://open` URL.
#[tauri::command]
pub async fn open_obsidian_note(vault_name: String, relative_path: String) -> Result<(), String> {
    if vault_name.trim().is_empty() || relative_path.trim().is_empty() {
        return Err("Missing vault or note path".to_string());
    }
    let url = obsidian_open_url(&vault_name, &relative_path);
    let status = std::process::Command::new("open")
        .arg(&url)
        .status()
        .map_err(|e| format!("Failed to open Obsidian: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open Obsidian. Is it installed?".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".obsidian")).unwrap();
        dir
    }

    fn config(vault: Option<&Path>, folder: &str) -> ObsidianExportConfig {
        ObsidianExportConfig {
            vault_path: vault.map(|path| path.display().to_string()),
            folder: folder.to_string(),
            ..ObsidianExportConfig::default()
        }
    }

    #[test]
    fn defaults_tag_and_transcript() {
        let config: ObsidianExportConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.tags, vec!["minutes-app"]);
        assert!(config.include_transcript);
        assert_eq!(config.vault_path, None);
    }

    #[test]
    fn folder_must_stay_inside_the_vault() {
        let vault = vault();
        let ok = validate_config(config(Some(vault.path()), "./Meetings//2026/")).unwrap();
        assert_eq!(ok.config.folder, "Meetings/2026");
        assert_eq!(ok.vault_warning, None);

        assert!(validate_config(config(Some(vault.path()), "../elsewhere")).is_err());
        assert!(validate_config(config(Some(vault.path()), "Meetings/../../x")).is_err());
        assert!(validate_config(config(Some(vault.path()), "/tmp")).is_err());
        assert!(validate_config(config(None, "..")).is_err());
    }

    #[test]
    fn vault_must_exist_and_warns_without_dot_obsidian() {
        let missing = config(Some(Path::new("/definitely/not/a/vault")), "");
        assert!(validate_config(missing).is_err());

        let plain = tempfile::tempdir().unwrap();
        let state = validate_config(config(Some(plain.path()), "")).unwrap();
        assert!(state.vault_warning.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_folder_leading_out_of_the_vault_is_refused() {
        let vault = vault();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.path().join("escape")).unwrap();
        assert!(validate_config(config(Some(vault.path()), "escape")).is_err());
        assert!(save_into_vault(vault.path(), "escape", "a.md", "x").is_err());
    }

    #[test]
    fn tags_are_normalised_for_obsidian() {
        assert_eq!(
            normalise_tags(&[
                "#minutes-app",
                " team standup ",
                "2026",
                "",
                "ok/nested",
                "Ünïcode!",
                "MINUTES-APP",
                "#"
            ]),
            vec!["minutes-app", "team-standup", "ok/nested", "Ünïcode"]
        );
    }

    #[test]
    fn saving_again_makes_new_versions_and_leaves_earlier_ones_alone() {
        let vault = vault();
        let first =
            save_into_vault(vault.path(), "Meetings", "Standup-2026-11-27.md", "one").unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(first.relative_path, "Meetings/Standup-2026-11-27.md");

        // The user edits the note in Obsidian.
        std::fs::write(&first.path, "edited in obsidian").unwrap();

        let second =
            save_into_vault(vault.path(), "Meetings", "Standup-2026-11-27.md", "two").unwrap();
        let third =
            save_into_vault(vault.path(), "Meetings", "Standup-2026-11-27.md", "three").unwrap();
        assert_eq!((second.version, third.version), (2, 3));
        assert_eq!(second.relative_path, "Meetings/Standup-2026-11-27 v2.md");
        assert_eq!(third.relative_path, "Meetings/Standup-2026-11-27 v3.md");

        assert_eq!(std::fs::read(&first.path).unwrap(), b"edited in obsidian");
        assert_eq!(std::fs::read_to_string(&second.path).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(&third.path).unwrap(), "three");
        assert!(vault.path().file_name().is_some());
        assert_eq!(
            first.vault_name,
            vault.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn file_names_are_sanitised_into_the_folder() {
        let vault = vault();
        let saved = save_into_vault(vault.path(), "", "../../evil/name", "x").unwrap();
        assert_eq!(saved.relative_path, "_.._evil_name.md");
        assert!(Path::new(&saved.path).starts_with(vault.path()));
    }

    #[test]
    fn open_url_encodes_like_encode_uri_component() {
        assert_eq!(
            obsidian_open_url("My Vault", "Meetings/Café plan v2.md"),
            "obsidian://open?vault=My%20Vault&file=Meetings%2FCaf%C3%A9%20plan%20v2.md"
        );
        assert_eq!(
            obsidian_open_url("a&b", "x?y#z"),
            "obsidian://open?vault=a%26b&file=x%3Fy%23z"
        );
    }

    #[test]
    fn picked_folder_is_returned_relative_to_the_vault() {
        let vault = vault();
        let nested = vault.path().join("Work/Meetings");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            relative_to_vault(vault.path(), &nested).unwrap(),
            "Work/Meetings"
        );
        assert_eq!(relative_to_vault(vault.path(), vault.path()).unwrap(), "");
        let outside = tempfile::tempdir().unwrap();
        assert!(relative_to_vault(vault.path(), outside.path()).is_err());
    }
}
