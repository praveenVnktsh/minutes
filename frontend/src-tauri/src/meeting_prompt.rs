//! Show the floating "meeting detected" prompt.
//!
//! Driven from Rust (not the main webview) so it appears even when the Minutes
//! window is in the background and macOS has throttled its webview.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Runtime};

/// Set when the user dismisses (or acts on) the prompt for the current call.
static PROMPT_DISMISSED: AtomicBool = AtomicBool::new(false);
static PENDING_REQUEST: LazyLock<Mutex<Option<RecordingRequest>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordingRequestStatus {
    Pending,
    Claimed,
    Starting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingRequest {
    pub request_id: String,
    pub source: String,
    pub status: RecordingRequestStatus,
    pub claimed_by: Option<String>,
    pub started_session_id: Option<String>,
}

fn pending_or_insert(pending: &mut Option<RecordingRequest>, source: &str) -> RecordingRequest {
    pending
        .get_or_insert_with(|| RecordingRequest {
            request_id: format!("recording-request-{}", uuid::Uuid::new_v4()),
            source: source.to_string(),
            status: RecordingRequestStatus::Pending,
            claimed_by: None,
            started_session_id: None,
        })
        .clone()
}

pub fn request_recording<R: Runtime>(app: &AppHandle<R>, source: &str) -> RecordingRequest {
    let request = {
        let mut pending = PENDING_REQUEST
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending_or_insert(&mut pending, source)
    };
    if request.status == RecordingRequestStatus::Pending {
        install_legacy_fallback(app, &request.request_id);
        if let Err(error) = app.emit("recording-start-requested", &request) {
            log::warn!("Failed to emit recording start request: {error}");
        }
    }
    request
}

fn install_legacy_fallback<R: Runtime>(app: &AppHandle<R>, request_id: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let id = serde_json::to_string(request_id).unwrap_or_else(|_| "null".to_string());
        let script = format!(
            "sessionStorage.setItem('pendingRecordingRequestId', {id});sessionStorage.setItem('autoStartRecording', 'true')"
        );
        let _ = window.eval(script);
    }
}

fn clear_legacy_fallback<R: Runtime>(app: &AppHandle<R>, request_id: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let id = serde_json::to_string(request_id).unwrap_or_else(|_| "null".to_string());
        let script = format!(
            "if(sessionStorage.getItem('pendingRecordingRequestId')==={id}){{sessionStorage.removeItem('autoStartRecording');sessionStorage.removeItem('pendingRecordingRequestId')}}"
        );
        let _ = window.eval(script);
    }
}

/// Dismiss the prompt for the current call (until audio stops).
pub fn dismiss() {
    PROMPT_DISMISSED.store(true, Ordering::SeqCst);
}

/// Allow prompting again (called when system audio stops).
pub fn reset() {
    PROMPT_DISMISSED.store(false, Ordering::SeqCst);
}

#[tauri::command]
pub fn get_pending_recording_request() -> Option<RecordingRequest> {
    PENDING_REQUEST
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn claim_request(request_id: &str, claimant: &str) -> Result<RecordingRequest, String> {
    let mut pending = PENDING_REQUEST
        .lock()
        .map_err(|_| "Recording request lock failed")?;
    claim_request_state(&mut pending, request_id, claimant)
}

fn claim_request_state(
    pending: &mut Option<RecordingRequest>,
    request_id: &str,
    claimant: &str,
) -> Result<RecordingRequest, String> {
    let request = pending
        .as_mut()
        .ok_or_else(|| "No recording request is pending".to_string())?;
    if request.request_id != request_id {
        return Err("Recording request is stale".to_string());
    }
    if request.status != RecordingRequestStatus::Pending {
        return Err("Recording request is already claimed".to_string());
    }
    request.status = RecordingRequestStatus::Claimed;
    request.claimed_by = Some(claimant.to_string());
    Ok(request.clone())
}

fn cancel_unclaimed(pending: &mut Option<RecordingRequest>) -> Option<String> {
    if pending.as_ref().map(|request| &request.status) == Some(&RecordingRequestStatus::Pending) {
        pending.take().map(|request| request.request_id)
    } else {
        None
    }
}

#[tauri::command]
pub fn claim_recording_request<R: Runtime>(
    app: AppHandle<R>,
    request_id: String,
    claimant: String,
) -> Result<RecordingRequest, String> {
    let request = claim_request(&request_id, &claimant)?;
    clear_legacy_fallback(&app, &request_id);
    Ok(request)
}

pub fn claim_legacy_recording_request() -> Result<Option<String>, String> {
    let request_id = get_pending_recording_request().map(|request| request.request_id);
    let Some(request_id) = request_id else {
        return Ok(None);
    };
    claim_request(&request_id, "legacy")?;
    begin_recording_request(&request_id)?;
    Ok(Some(request_id))
}

pub fn begin_recording_request(request_id: &str) -> Result<(), String> {
    let mut pending = PENDING_REQUEST
        .lock()
        .map_err(|_| "Recording request lock failed")?;
    begin_recording_request_state(&mut pending, request_id)
}

fn begin_recording_request_state(
    pending: &mut Option<RecordingRequest>,
    request_id: &str,
) -> Result<(), String> {
    let request = pending
        .as_mut()
        .ok_or_else(|| "No recording request is pending".to_string())?;
    if request.request_id != request_id {
        return Err("Recording request is stale".to_string());
    }
    if request.status == RecordingRequestStatus::Starting {
        return Err("Recording request is already starting".to_string());
    }
    request.status = RecordingRequestStatus::Starting;
    Ok(())
}

pub fn associate_started_session(request_id: &str, session_id: &str) -> Result<(), String> {
    let mut pending = PENDING_REQUEST
        .lock()
        .map_err(|_| "Recording request lock failed")?;
    associate_started_session_state(&mut pending, request_id, session_id)
}

fn associate_started_session_state(
    pending: &mut Option<RecordingRequest>,
    request_id: &str,
    session_id: &str,
) -> Result<(), String> {
    let request = pending
        .as_mut()
        .ok_or_else(|| "No recording request is pending".to_string())?;
    if request.request_id != request_id || request.status != RecordingRequestStatus::Starting {
        return Err("Recording request is not the active start".to_string());
    }
    request.started_session_id = Some(session_id.to_string());
    Ok(())
}

fn matches_started_session(request: &RecordingRequest, current_session_id: Option<&str>) -> bool {
    request.started_session_id.as_deref() == current_session_id && current_session_id.is_some()
}

/// Tauri command so the webview can dismiss the prompt.
#[tauri::command]
pub fn dismiss_meeting_prompt<R: Runtime>(app: AppHandle<R>) {
    dismiss();
    let request_id = {
        let mut pending = PENDING_REQUEST
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cancel_unclaimed(&mut pending)
    };
    if let Some(request_id) = request_id {
        clear_legacy_fallback(&app, &request_id);
    }
}

/// Position and show the `meeting-prompt` window for a detected app.
pub async fn maybe_show<R: Runtime>(app: &AppHandle<R>, app_name: &str) {
    if PROMPT_DISMISSED.load(Ordering::SeqCst) {
        return;
    }

    let enabled = crate::audio::recording_preferences::load_recording_preferences(app)
        .await
        .map(|prefs| prefs.automatic_record_prompt)
        .unwrap_or(true);
    if !enabled || crate::audio::recording_commands::is_recording().await {
        return;
    }

    let Some(window) = app.get_webview_window("meeting-prompt") else {
        log::warn!("Meeting prompt: window 'meeting-prompt' is unavailable");
        return;
    };

    if let Ok(Some(monitor)) = app.primary_monitor() {
        if let Ok(size) = window.outer_size() {
            let margin = (16.0 * monitor.scale_factor()) as i32;
            let x = monitor.position().x + monitor.size().width as i32 - size.width as i32 - margin;
            let y = monitor.position().y + margin;
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
    }

    if let Err(error) = window.show() {
        log::warn!("Meeting prompt: failed to show window: {}", error);
    }
    if let Err(error) = app.emit_to(
        "meeting-prompt",
        "meeting-prompt-show",
        serde_json::json!({ "appName": app_name }),
    ) {
        log::warn!("Meeting prompt: failed to emit show event: {}", error);
    }
    let _ = window.set_focus();
}

/// Start recording from the floating prompt. Runs entirely in Rust so it does
/// not depend on the (possibly throttled) main webview.
#[tauri::command]
pub async fn start_recording_from_prompt<R: Runtime>(app: AppHandle<R>) -> RecordingRequest {
    let request = request_recording(&app, "meeting_prompt");
    crate::tray::focus_main_window(&app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval("window.location.assign('/')");
    }
    request
}

#[tauri::command]
pub fn acknowledge_recording_request<R: Runtime>(
    app: AppHandle<R>,
    request_id: String,
    accepted: bool,
    error: Option<String>,
) -> Result<(), String> {
    let mut pending = PENDING_REQUEST
        .lock()
        .map_err(|_| "Recording request lock failed")?;
    if pending.as_ref().map(|request| request.request_id.as_str()) != Some(request_id.as_str()) {
        return Err("Recording request is stale".to_string());
    }
    let request = pending.as_ref().expect("checked above");
    let started_session_id = request.started_session_id.clone();
    if accepted {
        let current_session_id = crate::meeting_activity::recording_identity()
            .filter(|recording| {
                matches!(
                    recording.status,
                    crate::meeting_activity::ActivityStatus::Recording
                        | crate::meeting_activity::ActivityStatus::Paused
                )
            })
            .map(|recording| recording.session_id);
        if !matches_started_session(request, current_session_id.as_deref()) {
            return Err("Recording request cannot be accepted before recording starts".to_string());
        }
    }
    *pending = None;
    drop(pending);
    clear_legacy_fallback(&app, &request_id);
    let result = serde_json::json!({
        "request_id": request_id,
        "accepted": accepted,
        "error": error,
        "session_id": started_session_id,
    });
    if let Err(error) = app.emit_to("meeting-prompt", "meeting-prompt-start-result", result) {
        log::warn!("Failed to emit meeting prompt result: {error}");
    }
    if accepted {
        dismiss();
        if let Some(window) = app.get_webview_window("meeting-prompt") {
            let _ = window.hide();
        }
    } else {
        crate::tray::set_tray_state(&app, crate::tray::RecordingState::Stopped);
        crate::tray::update_tray_menu(&app);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_requests_reuse_the_pending_identity() {
        let mut pending = None;
        let first = pending_or_insert(&mut pending, "tray");
        let second = pending_or_insert(&mut pending, "meeting_prompt");

        assert_eq!(first.request_id, second.request_id);
        assert_eq!(second.source, "tray");
    }

    #[test]
    fn only_one_consumer_can_claim_a_request() {
        let mut pending = None;
        let request = pending_or_insert(&mut pending, "tray");
        let claimed = claim_request_state(&mut pending, &request.request_id, "controller").unwrap();

        assert_eq!(claimed.status, RecordingRequestStatus::Claimed);
        assert_eq!(claimed.claimed_by.as_deref(), Some("controller"));
        assert!(claim_request_state(&mut pending, &request.request_id, "legacy").is_err());
    }

    #[test]
    fn dismiss_does_not_erase_an_in_flight_request() {
        let mut pending = None;
        let request = pending_or_insert(&mut pending, "tray");
        claim_request_state(&mut pending, &request.request_id, "controller").unwrap();

        assert!(cancel_unclaimed(&mut pending).is_none());
        assert_eq!(pending.unwrap().request_id, request.request_id);
    }

    #[test]
    fn success_requires_the_session_started_for_that_request() {
        let mut pending = None;
        let request = pending_or_insert(&mut pending, "tray");
        claim_request_state(&mut pending, &request.request_id, "controller").unwrap();
        begin_recording_request_state(&mut pending, &request.request_id).unwrap();
        associate_started_session_state(&mut pending, &request.request_id, "session-a").unwrap();
        let request = pending.as_ref().unwrap();

        assert!(matches_started_session(request, Some("session-a")));
        assert!(!matches_started_session(request, Some("session-b")));
    }
}
