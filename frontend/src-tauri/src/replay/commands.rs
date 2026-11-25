//! Tauri commands for the in-app replay viewer (Settings → Developer →
//! Replays): the run index, a run's files, its recording and the live Copilot.
//!
//! They only read run folders directly under `<app data>/replays`
//! ([`runs::run_dir`]), open a run's sandbox database read-only and write
//! nothing but the run's `interactive.jsonl`. They never touch recording,
//! transcription, summaries or the meetings database.
//!
//! The Developer setting is enforced here, not only in the UI: it is persisted
//! in `store.json` (`developerReplays`), and every command but the cancel
//! refuses while it is off. A live ask is refused while a meeting is being
//! recorded (or is stopping), and one running when a recording starts is
//! stopped, so a replay never takes the model from the real meeting's live
//! summary or Copilot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_store::StoreExt;

use super::live_ask::LiveAsker;
use super::runs;

/// The event live Copilot progress arrives on:
/// `{requestId, run, event, data}`.
pub const REPLAY_COPILOT_EVENT: &str = "replay-copilot";

/// Where the Developer → Replays setting is kept in `store.json`.
pub const ENABLED_KEY: &str = "developerReplays";

const OFF: &str = "Replays are off. Turn them on in Settings → Developer → Replays.";
const RECORDING: &str =
    "Live replay asks wait until the meeting being recorded is saved, so they never take the model from its live summary or Copilot.";

/// The live Copilot shared by the viewer's asks, their cancel flags, and
/// whether the Developer setting allows replays at all (off until loaded).
#[derive(Default)]
pub struct ReplayState {
    pub asker: Arc<LiveAsker>,
    pub cancelled: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub enabled: AtomicBool,
}

impl ReplayState {
    /// `Err` (for the page) unless the Developer setting allows replays.
    pub fn require_enabled(&self) -> Result<(), String> {
        if self.enabled.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(OFF.to_string())
        }
    }

    /// Switches replays on or off; off also abandons every ask.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        if !enabled {
            self.cancel_all();
        }
    }

    fn cancel_all(&self) {
        for flag in self
            .cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            flag.store(true, Ordering::Release);
        }
        self.asker.stop_any();
    }
}

/// Refuses a live ask while a meeting is being recorded or saved.
pub fn refuse_while_recording(recording: bool) -> Result<(), String> {
    if recording {
        Err(RECORDING.to_string())
    } else {
        Ok(())
    }
}

/// Restores the persisted Developer → Replays setting at startup.
pub fn load<R: Runtime>(app: &AppHandle<R>) {
    let enabled = app
        .store("store.json")
        .ok()
        .and_then(|store| store.get(ENABLED_KEY))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    app.state::<ReplayState>().set_enabled(enabled);
}

#[tauri::command]
pub fn get_replays_enabled(state: State<'_, ReplayState>) -> bool {
    state.enabled.load(Ordering::Acquire)
}

#[tauri::command]
pub fn set_replays_enabled<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ReplayState>,
    enabled: bool,
) -> Result<(), String> {
    state.set_enabled(enabled);
    let store = app
        .store("store.json")
        .map_err(|e| format!("Could not save the setting: {e}"))?;
    store.set(ENABLED_KEY, Value::Bool(enabled));
    store
        .save()
        .map_err(|e| format!("Could not save the setting: {e}"))?;
    log::info!("Developer replays set to {enabled}");
    Ok(())
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
    runs::list_runs(root)
}

fn run_folder(root: &Path, run: &str) -> Result<PathBuf> {
    runs::run_dir(root, run).ok_or_else(|| anyhow!("No replay run named {run:?}"))
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
    Ok(runs::run_audio(&run_folder(root, run)?)
        .ok()
        .map(|(path, _)| path))
}

#[tauri::command]
pub async fn replay_list_runs<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ReplayState>,
) -> Result<Value, String> {
    state.require_enabled()?;
    let root = replays_root(&app).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || list_runs_in(&root))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn replay_read_file<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ReplayState>,
    run: String,
    file: String,
) -> Result<tauri::ipc::Response, String> {
    state.require_enabled()?;
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
    state: State<'_, ReplayState>,
    run: String,
) -> Result<Option<String>, String> {
    state.require_enabled()?;
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
    state.require_enabled()?;
    refuse_while_recording(crate::audio::recording_commands::is_recording_or_stopping())?;
    let root = replays_root(&app).map_err(|e| e.to_string())?;
    let dir = run_folder(&root, &run).map_err(|e| e.to_string())?;
    let body = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let ask = runs::parse_ask(&body)?;
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
        let mut stopped_for_recording = false;
        move |event: &str, data: Value| {
            if cancelled.load(Ordering::Acquire) || stopped_for_recording {
                return false;
            }
            // A recording started meanwhile: the meeting gets the model.
            let (event, data) = if crate::audio::recording_commands::is_recording_or_stopping() {
                stopped_for_recording = true;
                ("error", json!({ "message": RECORDING }))
            } else {
                (event, data)
            };
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
            !stopped_for_recording
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
pub fn replay_copilot_stop(state: State<'_, ReplayState>, run: String) -> Result<bool, String> {
    state.require_enabled()?;
    Ok(state.asker.stop(&run))
}

/// Leaving the replay page: abandons every ask and stops the running answer.
/// Allowed even when replays are off: it only ever stops work.
#[tauri::command]
pub fn replay_copilot_cancel(state: State<'_, ReplayState>) {
    state.cancel_all();
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
    fn replays_are_refused_until_the_setting_is_on() {
        let state = ReplayState::default();
        assert!(state
            .require_enabled()
            .unwrap_err()
            .contains("Settings → Developer"));
        state.set_enabled(true);
        assert!(state.require_enabled().is_ok());
        let flag = Arc::new(AtomicBool::new(false));
        state
            .cancelled
            .lock()
            .unwrap()
            .insert("a".into(), flag.clone());
        state.set_enabled(false);
        assert!(state.require_enabled().is_err());
        assert!(
            flag.load(Ordering::Acquire),
            "turning replays off abandons the running ask"
        );
    }

    #[test]
    fn live_asks_wait_for_a_recording_to_be_saved() {
        assert!(refuse_while_recording(false).is_ok());
        assert!(refuse_while_recording(true)
            .unwrap_err()
            .contains("being recorded"));
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
        std::os::unix::fs::symlink(&real, meeting.join("audio.mp4")).unwrap();
        let found = audio_for(root.path(), run).unwrap().unwrap();
        assert_eq!(found, real.canonicalize().unwrap(), "the link is followed");
        assert!(found.is_file());
    }
}
