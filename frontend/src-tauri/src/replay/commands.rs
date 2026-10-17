//! Tauri commands for the in-app replay viewer (Settings → Developer →
//! Replays): the run index, a run's files, its recording and the live Copilot.
//!
//! They only read run folders directly under `<app data>/replays`
//! ([`serve::run_dir`]), open a run's sandbox database read-only and write
//! nothing but the run's `interactive.jsonl`. They never touch recording,
//! transcription, summaries or the meetings database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use super::live_ask::LiveAsker;
use super::serve;

/// The event live Copilot progress arrives on:
/// `{requestId, run, event, data}`.
pub const REPLAY_COPILOT_EVENT: &str = "replay-copilot";

/// The live Copilot shared by the viewer's asks, and their cancel flags.
#[derive(Default)]
pub struct ReplayState {
    pub asker: Arc<LiveAsker>,
    pub cancelled: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn replays_root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .context("the app data folder is unavailable")?
        .join("replays"))
}

/// The runs under `root` (`{runs: [...]}`, newest first). A missing root has none.
pub fn list_runs_in(root: &Path) -> Value {
    if !root.is_dir() {
        return json!({ "runs": [] });
    }
    serve::list_runs(root)
}

fn run_folder(root: &Path, run: &str) -> Result<PathBuf> {
    serve::run_dir(root, run).ok_or_else(|| anyhow!("No replay run named {run:?}"))
}

/// One of a run's files: `events` (required), `report` or `interactive`
/// (empty when the run has none yet).
pub fn read_run_file(root: &Path, run: &str, file: &str) -> Result<Vec<u8>> {
    let dir = run_folder(root, run)?;
    let (name, required) = match file {
        "events" => ("events.jsonl", true),
        "report" => ("report.json", false),
        "interactive" => ("interactive.jsonl", false),
        other => return Err(anyhow!("Unknown replay file {other:?}")),
    };
    match std::fs::read(dir.join(name)) {
        Ok(body) => Ok(body),
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow!("Could not read {name} of {run}: {e}")),
    }
}

/// The real file of the run's recording, if it has one.
pub fn audio_for(root: &Path, run: &str) -> Result<Option<PathBuf>> {
    Ok(serve::recording_file(&run_folder(root, run)?))
}

#[tauri::command]
pub async fn replay_list_runs<R: Runtime>(app: AppHandle<R>) -> Result<Value, String> {
    let root = replays_root(&app).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || list_runs_in(&root))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn replay_read_file<R: Runtime>(
    app: AppHandle<R>,
    run: String,
    file: String,
) -> Result<tauri::ipc::Response, String> {
    let root = replays_root(&app).map_err(|e| e.to_string())?;
    let body = tauri::async_runtime::spawn_blocking(move || read_run_file(&root, &run, &file))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))?;
    Ok(tauri::ipc::Response::new(body))
}

/// The run's recording path, allowed (that file only) on the asset protocol
/// so the page can stream it with `convertFileSrc`.
#[tauri::command]
pub async fn replay_audio_path<R: Runtime>(
    app: AppHandle<R>,
    run: String,
) -> Result<Option<String>, String> {
    let root = replays_root(&app).map_err(|e| e.to_string())?;
    let Some(path) = audio_for(&root, &run).map_err(|e| format!("{e:#}"))? else {
        return Ok(None);
    };
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(|e| format!("Could not allow the recording: {e}"))?;
    Ok(Some(path.to_string_lossy().to_string()))
}

/// Asks the real Copilot about `run` as it stood at the request's cursor
/// (`{audio_ms, seq?, kind: "ask"|"action", text?, action?}`). Progress is
/// emitted as `replay-copilot` events; returns when the answer ends.
#[tauri::command]
pub async fn replay_copilot_ask<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ReplayState>,
    run: String,
    request: Value,
    request_id: String,
) -> Result<(), String> {
    let root = replays_root(&app).map_err(|e| e.to_string())?;
    let dir = run_folder(&root, &run).map_err(|e| e.to_string())?;
    let body = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let ask = serve::parse_ask(&body)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    state
        .cancelled
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(request_id.clone(), cancelled.clone());
    let sink = {
        let app = app.clone();
        let run = run.clone();
        let request_id = request_id.clone();
        move |event: &str, data: Value| {
            if cancelled.load(Ordering::Acquire) {
                return false;
            }
            if event != "ping" {
                let payload = json!({
                    "requestId": request_id,
                    "run": run,
                    "event": event,
                    "data": data,
                });
                if let Err(error) = app.emit(REPLAY_COPILOT_EVENT, payload) {
                    log::warn!("replay live copilot: could not emit {event}: {error}");
                }
            }
            true
        }
    };
    let asker = state.asker.clone();
    let result = asker.ask(&run, &dir, ask, sink).await;
    state
        .cancelled
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&request_id);
    result.map_err(|e| {
        log::warn!("replay live copilot: ask on {run} failed: {e:#}");
        format!("{e:#}")
    })
}

/// Stops the running answer of `run` inside the helper.
#[tauri::command]
pub fn replay_copilot_stop(state: State<'_, ReplayState>, run: String) -> bool {
    state.asker.stop(&run)
}

/// Leaving the replay page: abandons every ask and stops the running answer.
#[tauri::command]
pub fn replay_copilot_cancel(state: State<'_, ReplayState>) {
    for flag in state
        .cancelled
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
    {
        flag.store(true, Ordering::Release);
    }
    state.asker.stop_any();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_with_run() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20261016-100000-a");
        std::fs::create_dir_all(run.join("sandbox").join("meeting")).unwrap();
        std::fs::write(
            run.join("events.jsonl"),
            "{\"seq\":0,\"t_ms\":0,\"audio_ms\":0,\"type\":\"meta\",\"name\":\"run-start\",\"payload\":{\"audio_name\":\"audio.mp4\",\"duration_s\":60}}\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn runs_are_indexed_and_a_missing_root_has_none() {
        let root = root_with_run();
        let list = list_runs_in(root.path());
        assert_eq!(list["runs"].as_array().unwrap().len(), 1);
        assert_eq!(list["runs"][0]["name"], "20261016-100000-a");
        assert_eq!(list["runs"][0]["duration_s"], 60);
        let missing = list_runs_in(&root.path().join("nope"));
        assert_eq!(missing["runs"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn run_files_are_read_and_optional_ones_may_be_missing() {
        let root = root_with_run();
        let run = "20261016-100000-a";
        let events = read_run_file(root.path(), run, "events").unwrap();
        assert!(String::from_utf8(events).unwrap().contains("run-start"));
        assert!(read_run_file(root.path(), run, "report")
            .unwrap()
            .is_empty());
        assert!(read_run_file(root.path(), run, "interactive")
            .unwrap()
            .is_empty());
        std::fs::write(root.path().join(run).join("interactive.jsonl"), "{}\n").unwrap();
        assert_eq!(
            read_run_file(root.path(), run, "interactive").unwrap(),
            b"{}\n"
        );
        assert!(read_run_file(root.path(), run, "app.log").is_err());
        std::fs::remove_file(root.path().join(run).join("events.jsonl")).unwrap();
        assert!(read_run_file(root.path(), run, "events").is_err());
    }

    #[test]
    fn nothing_outside_the_root_is_read() {
        let root = root_with_run();
        std::fs::write(root.path().join("secret.txt"), "x").unwrap();
        for bad in ["..", "../x", "a/b", ".hidden", "secret.txt", "", "missing"] {
            assert!(read_run_file(root.path(), bad, "events").is_err(), "{bad}");
            assert!(audio_for(root.path(), bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_recording_is_the_linked_media_file() {
        let root = root_with_run();
        let run = "20261016-100000-a";
        assert_eq!(audio_for(root.path(), run).unwrap(), None);
        let meeting = root.path().join(run).join("sandbox").join("meeting");
        std::fs::write(meeting.join("transcripts.json"), "[]").unwrap();
        assert_eq!(audio_for(root.path(), run).unwrap(), None);
        let elsewhere = tempfile::tempdir().unwrap();
        let real = elsewhere.path().join("audio.mp4");
        std::fs::write(&real, b"media").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, meeting.join("audio.mp4")).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&real, meeting.join("audio.mp4")).unwrap();
        let found = audio_for(root.path(), run).unwrap().unwrap();
        #[cfg(unix)]
        assert_eq!(found, real.canonicalize().unwrap(), "the link is followed");
        assert!(found.is_file());
    }
}
