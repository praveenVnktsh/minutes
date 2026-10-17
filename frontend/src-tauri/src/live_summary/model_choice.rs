//! Which model live notes run on, and the opt-in download of the live notes
//! model (Gemma 4 E2B).
//!
//! Nothing here downloads without the user asking: onboarding's checkbox or
//! the Settings section calls in. An onboarding opt-in waits until the
//! required models (Parakeet and the summary model) are on disk, survives
//! the app reloading or restarting, and is dropped if the user later picks
//! the summary model for live notes.

use std::time::Duration;

use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use super::llm::{is_low_memory, LiveNotesChoice};
use super::STORE_FILE;
use crate::summary::summary_engine::client::total_memory_bytes;
use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;

/// `"summary"` (or absent) or the live notes model's name.
pub const MODEL_KEY: &str = "liveNotesModel";
/// The summary model an onboarding opt-in waits for before the live notes
/// model downloads; absent when nothing is pending.
pub const PENDING_KEY: &str = "liveNotesDownloadAfter";

/// How often a pending download checks whether the required models are in.
const PENDING_POLL: Duration = Duration::from_secs(10);

/// Onboarding ticks the live notes download by default from this much
/// memory: on 8 GB Macs two resident models do not fit while recording.
const OPT_IN_BY_DEFAULT_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LiveNotesSettings {
    /// `"summary"` or the live notes model's name.
    pub model: String,
    /// The live notes model's name, for the UI.
    pub live_notes_model: &'static str,
    /// An 8 GB Mac: live notes stay on the summary model while recording.
    pub low_memory: bool,
    /// Whether onboarding ticks the download by default.
    pub opt_in_by_default: bool,
    /// An onboarding opt-in still waiting for the required models.
    pub download_pending: bool,
}

/// Onboarding's default for the live notes checkbox: on with 16 GB or more,
/// off at 8 GB or less (and when the memory is unknown).
pub fn opt_in_by_default(total_memory: Option<u64>) -> bool {
    total_memory.is_some_and(|bytes| bytes >= OPT_IN_BY_DEFAULT_BYTES)
}

/// What a pending onboarding opt-in does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingStep {
    /// The required models are not all on disk yet.
    Wait,
    /// Start the live notes download.
    Start,
    /// Already downloaded: nothing to fetch.
    Done,
}

pub fn pending_step(
    parakeet_ready: bool,
    summary_ready: bool,
    live_notes_ready: bool,
) -> PendingStep {
    if live_notes_ready {
        PendingStep::Done
    } else if parakeet_ready && summary_ready {
        PendingStep::Start
    } else {
        PendingStep::Wait
    }
}

fn read_string<R: Runtime>(app: &AppHandle<R>, key: &str) -> Option<String> {
    let store = app.store(STORE_FILE).ok()?;
    store
        .get(key)
        .and_then(|value| value.as_str().map(str::to_string))
}

fn write<R: Runtime>(app: &AppHandle<R>, key: &str, value: Option<&str>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    match value {
        Some(value) => store.set(key, serde_json::json!(value)),
        None => {
            store.delete(key);
        }
    }
    store.save().map_err(|e| e.to_string())
}

/// The stored live notes choice.
pub fn choice<R: Runtime>(app: &AppHandle<R>) -> LiveNotesChoice {
    LiveNotesChoice::from_stored(read_string(app, MODEL_KEY).as_deref())
}

fn settings<R: Runtime>(app: &AppHandle<R>) -> LiveNotesSettings {
    let memory = total_memory_bytes();
    LiveNotesSettings {
        model: choice(app).as_str().to_string(),
        live_notes_model: LIVE_NOTES_MODEL,
        low_memory: is_low_memory(memory),
        opt_in_by_default: opt_in_by_default(memory),
        download_pending: read_string(app, PENDING_KEY).is_some(),
    }
}

#[tauri::command]
pub async fn get_live_notes_settings<R: Runtime>(app: AppHandle<R>) -> LiveNotesSettings {
    settings(&app)
}

/// Sets the live notes model: `"summary"` or the live notes model. Choosing
/// the summary model also drops a pending onboarding download. Never
/// downloads anything: the Settings section downloads on its own button.
#[tauri::command]
pub async fn set_live_notes_model<R: Runtime>(
    app: AppHandle<R>,
    model: String,
) -> Result<LiveNotesSettings, String> {
    let choice = match model.as_str() {
        LiveNotesChoice::SUMMARY => LiveNotesChoice::SummaryModel,
        LIVE_NOTES_MODEL => LiveNotesChoice::LiveNotesModel,
        other => return Err(format!("Unknown live notes model: {other}")),
    };
    write(&app, MODEL_KEY, Some(choice.as_str()))?;
    if choice == LiveNotesChoice::SummaryModel {
        write(&app, PENDING_KEY, None)?;
    }
    Ok(settings(&app))
}

/// Onboarding's opt-in: live notes on the live notes model, downloaded once
/// Parakeet and `summary_model` are on disk.
#[tauri::command]
pub async fn request_live_notes_download_after_setup<R: Runtime>(
    app: AppHandle<R>,
    summary_model: String,
) -> Result<LiveNotesSettings, String> {
    write(&app, MODEL_KEY, Some(LIVE_NOTES_MODEL))?;
    write(&app, PENDING_KEY, Some(&summary_model))?;
    spawn_pending_download(app.clone());
    Ok(settings(&app))
}

/// At startup: resume an onboarding opt-in the last run did not finish.
pub fn resume_pending_download<R: Runtime>(app: &AppHandle<R>) {
    if read_string(app, PENDING_KEY).is_some() {
        spawn_pending_download(app.clone());
    }
}

fn spawn_pending_download<R: Runtime>(app: AppHandle<R>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    // One waiter per process: a second opt-in only updates the stored model.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        run_pending_download(&app).await;
        RUNNING.store(false, Ordering::SeqCst);
    });
}

async fn run_pending_download<R: Runtime>(app: &AppHandle<R>) {
    use crate::summary::summary_engine::commands::{
        builtin_ai_download_model, builtin_ai_is_model_ready, ModelManagerState,
    };
    loop {
        let Some(summary_model) = read_string(app, PENDING_KEY) else {
            return;
        };
        let parakeet_ready = crate::parakeet_engine::commands::parakeet_has_available_models()
            .await
            .unwrap_or(false);
        let ready = |model: String| {
            builtin_ai_is_model_ready(
                app.clone(),
                app.state::<ModelManagerState>(),
                model,
                Some(true),
            )
        };
        let summary_ready = ready(summary_model).await.unwrap_or(false);
        let live_notes_ready = ready(LIVE_NOTES_MODEL.to_string()).await.unwrap_or(false);
        match pending_step(parakeet_ready, summary_ready, live_notes_ready) {
            PendingStep::Wait => tokio::time::sleep(PENDING_POLL).await,
            PendingStep::Done => {
                let _ = write(app, PENDING_KEY, None);
                return;
            }
            PendingStep::Start => break,
        }
    }
    info!("Live notes: the required models are in; downloading the live notes model");
    let result = builtin_ai_download_model(
        app.clone(),
        app.state::<ModelManagerState>(),
        LIVE_NOTES_MODEL.to_string(),
    )
    .await;
    // Done either way: a failed or cancelled download is retried from
    // Settings, never in a loop behind the user's back.
    let _ = write(app, PENDING_KEY, None);
    if let Err(error) = result {
        warn!("Live notes model download did not finish: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn onboarding_ticks_the_download_from_16_gb() {
        assert!(!opt_in_by_default(Some(8 * GB)));
        assert!(!opt_in_by_default(Some(12 * GB)));
        assert!(opt_in_by_default(Some(16 * GB)));
        assert!(opt_in_by_default(Some(24 * GB)));
        assert!(!opt_in_by_default(None));
    }

    #[test]
    fn the_opt_in_download_waits_for_the_required_models() {
        assert_eq!(pending_step(false, false, false), PendingStep::Wait);
        assert_eq!(pending_step(true, false, false), PendingStep::Wait);
        assert_eq!(pending_step(false, true, false), PendingStep::Wait);
        assert_eq!(pending_step(true, true, false), PendingStep::Start);
        assert_eq!(pending_step(false, false, true), PendingStep::Done);
    }
}
