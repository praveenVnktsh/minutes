// audio/recording_commands.rs
//
// Slim Tauri command layer for recording functionality.
// Delegates to transcription and recording modules for actual implementation.

use anyhow::Result;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::task::JoinHandle;

use super::device_monitor::{DeviceEvent, DeviceMonitorType};
use super::{
    default_input_device,  // Get default microphone
    default_output_device, // Get default system audio
    parse_audio_device,
    recording_manager::RecordingStartError,
    RecordingManager,
};

// Import transcription modules
use super::transcription::{self, reset_speech_detected_flag};

// Re-export TranscriptUpdate for backward compatibility
pub use super::transcription::TranscriptUpdate;

// ============================================================================
// GLOBAL STATE
// ============================================================================

// Simple recording state tracking
static IS_RECORDING: AtomicBool = AtomicBool::new(false);

/// True for the whole `stop_recording` tail (manager taken -> `recording-stopped`
/// emitted). `IS_RECORDING` must stay true through the tail — the frontend polls
/// it to keep the stop UI up — so the mic-disconnect fallback checks this flag
/// too, otherwise a fallback queued before Stop retries against a taken manager
/// and surfaces a spurious "Microphone fallback failed" toast.
static IS_RECORDING_STOPPING: AtomicBool = AtomicBool::new(false);

/// Recording is live and not being torn down — the only state in which the
/// mic-disconnect fallback should run or report.
fn recording_live() -> bool {
    IS_RECORDING.load(Ordering::SeqCst) && !IS_RECORDING_STOPPING.load(Ordering::SeqCst)
}

fn stopped_capture_needing_cleanup() -> Option<String> {
    if !IS_RECORDING.load(Ordering::SeqCst) || IS_RECORDING_STOPPING.load(Ordering::SeqCst) {
        return None;
    }
    let manager = RECORDING_MANAGER.lock().unwrap();
    manager
        .as_ref()
        .and_then(RecordingSession::stopped_session_id)
}

async fn cleanup_stopped_capture<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if let Some(session_id) = stopped_capture_needing_cleanup() {
        let result = stop_recording_session(
            app.clone(),
            RecordingArgs {
                save_path: String::new(),
            },
            Some(session_id),
        )
        .await;
        if result.is_err() && has_recording_session() {
            return result;
        }
    }
    Ok(())
}

/// Recording is live AND the global manager is still the session `s` belongs to.
/// Used by the mic-disconnect fallback to refuse acting on a *later* recording
/// after a Stop/Start swapped the manager out from under an in-flight task.
///
/// NOTE: this locks `RECORDING_MANAGER`. Never call it while already holding
/// that lock (e.g. inside a `RECORDING_MANAGER.lock()` scope) — the std Mutex
/// is non-reentrant and it would self-deadlock. All current callers invoke it
/// outside any held lock; keep it that way.
fn session_live(s: &Arc<super::RecordingState>) -> bool {
    recording_live()
        && s.is_recording()
        && RECORDING_MANAGER
            .lock()
            .unwrap()
            .as_ref()
            .map_or(false, |m| Arc::ptr_eq(m.get_state(), s))
}

/// RAII guard for the stop-tail flag. Sets `IS_RECORDING_STOPPING` true on
/// construction and clears it on Drop — including during unwind — so a panic
/// anywhere in the ~320-line stop tail can't leave the flag stuck true and
/// silently kill the mic-disconnect fallback for every later recording.
///
/// This unwind-clears behaviour depends on `panic = "unwind"` (the default).
/// If a release profile ever sets `panic = "abort"`, Drop won't run on panic
/// and the stuck-flag failure mode returns — add a start-time reset then.
struct StoppingGuard;

fn claim_stop(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

fn session_matches(expected: Option<&str>, current: &str) -> bool {
    expected.map(|expected| expected == current).unwrap_or(true)
}

impl StoppingGuard {
    fn try_new() -> Result<Self, String> {
        claim_stop(&IS_RECORDING_STOPPING)
            .then(|| StoppingGuard)
            .ok_or_else(|| "Recording is already stopping".to_string())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::{
        completed_snapshot_after_success, CompletedTranscriptHistory, StoppingGuard,
        COMPLETED_TRANSCRIPT_HISTORY_LIMIT, IS_RECORDING_STOPPING,
    };
    use std::sync::atomic::Ordering;

    fn segment(sequence_id: u64) -> crate::audio::recording_saver::TranscriptSegment {
        crate::audio::recording_saver::TranscriptSegment {
            id: format!("segment-{sequence_id}"),
            text: format!("text-{sequence_id}"),
            audio_start_time: sequence_id as f64,
            audio_end_time: sequence_id as f64 + 1.0,
            duration: 1.0,
            display_time: format!("00:{sequence_id:02}"),
            confidence: 1.0,
            sequence_id,
            speaker: None,
        }
    }

    #[test]
    fn only_one_concurrent_stop_can_claim_the_session() {
        IS_RECORDING_STOPPING.store(false, Ordering::SeqCst);
        let owner = StoppingGuard::try_new().unwrap();
        assert!(StoppingGuard::try_new().is_err());
        assert!(IS_RECORDING_STOPPING.load(Ordering::SeqCst));
        drop(owner);
        assert!(!IS_RECORDING_STOPPING.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_session_cleanup_cannot_target_a_replacement_session() {
        assert!(super::session_matches(Some("failed-a"), "failed-a"));
        assert!(!super::session_matches(Some("failed-a"), "healthy-b"));
        assert!(super::session_matches(None, "manual-stop"));
    }

    #[test]
    fn cleanup_observation_uses_the_stopped_manager_slots_session() {
        let session = super::RecordingSession {
            manager: super::RecordingManager::new(),
            session_id: "failed-a".to_string(),
        };

        assert_eq!(session.stopped_session_id().as_deref(), Some("failed-a"));
    }

    #[test]
    fn completed_history_selects_only_the_exact_requested_session() {
        let mut history = CompletedTranscriptHistory::default();
        history.publish("session-a".to_string(), vec![segment(1)]);
        history.publish("session-b".to_string(), vec![segment(2)]);

        assert_eq!(history.get("session-a").unwrap()[0].sequence_id, 1);
        assert_eq!(history.get("session-b").unwrap()[0].sequence_id, 2);
        assert_eq!(
            history.get("unknown-session").unwrap_err(),
            "Transcript history is unavailable for recording session unknown-session"
        );
    }

    #[test]
    fn completed_history_evicts_old_sessions_without_cross_session_fallback() {
        let mut history = CompletedTranscriptHistory::default();
        for sequence_id in 0..=COMPLETED_TRANSCRIPT_HISTORY_LIMIT as u64 {
            history.publish(format!("session-{sequence_id}"), vec![segment(sequence_id)]);
        }

        assert!(history.get("session-0").is_err());
        assert_eq!(history.snapshots.len(), COMPLETED_TRANSCRIPT_HISTORY_LIMIT);
        assert_eq!(
            history
                .get(&format!("session-{COMPLETED_TRANSCRIPT_HISTORY_LIMIT}"))
                .unwrap()[0]
                .sequence_id,
            COMPLETED_TRANSCRIPT_HISTORY_LIMIT as u64
        );
    }

    #[test]
    fn failed_native_completion_cannot_publish_transcript_history() {
        assert!(completed_snapshot_after_success(
            &Some("save failed".to_string()),
            Some(vec![segment(4)]),
        )
        .is_none());
        assert_eq!(
            completed_snapshot_after_success(&None, Some(vec![segment(4)])).unwrap()[0].sequence_id,
            4
        );
    }
}
impl Drop for StoppingGuard {
    fn drop(&mut self) {
        IS_RECORDING_STOPPING.store(false, Ordering::SeqCst);
    }
}

/// Shared start-path finalize. Both start commands MUST call this so a new
/// start path can't silently ship with a per-session flag left unreset (e.g.
/// the mic-recovery budget already exhausted).
fn finalize_recording_start<R: Runtime>(app: &AppHandle<R>, session_id: String) {
    info!("🔍 Setting IS_RECORDING to true and resetting SPEECH_DETECTED_EMITTED");
    IS_RECORDING.store(true, Ordering::SeqCst);
    MIC_FALLBACK_FAILED_ATTEMPTS.store(0, Ordering::SeqCst); // fresh mic-recovery budget per session
    reset_speech_detected_flag(); // reset speech-detected emit latch for the new session
    crate::meeting_activity::set_recording_status(
        app,
        &session_id,
        crate::meeting_activity::ActivityStatus::Recording,
    );
}

struct RecordingSession {
    manager: RecordingManager,
    session_id: String,
}

impl RecordingSession {
    fn stopped_session_id(&self) -> Option<String> {
        (!self.manager.get_state().is_recording()).then(|| self.session_id.clone())
    }
}

impl Deref for RecordingSession {
    type Target = RecordingManager;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

impl DerefMut for RecordingSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.manager
    }
}

// Global recording manager and transcription task to keep them alive during recording
static RECORDING_MANAGER: Mutex<Option<RecordingSession>> = Mutex::new(None);
static TRANSCRIPTION_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

const COMPLETED_TRANSCRIPT_HISTORY_LIMIT: usize = 50;

#[derive(Default)]
struct CompletedTranscriptHistory {
    snapshots: VecDeque<(
        String,
        Arc<Vec<crate::audio::recording_saver::TranscriptSegment>>,
    )>,
}

impl CompletedTranscriptHistory {
    fn publish(
        &mut self,
        session_id: String,
        segments: Vec<crate::audio::recording_saver::TranscriptSegment>,
    ) {
        if let Some(index) = self
            .snapshots
            .iter()
            .position(|(stored_session_id, _)| stored_session_id == &session_id)
        {
            self.snapshots.remove(index);
        }
        self.snapshots.push_back((session_id, Arc::new(segments)));
        while self.snapshots.len() > COMPLETED_TRANSCRIPT_HISTORY_LIMIT {
            self.snapshots.pop_front();
        }
    }

    fn get(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::audio::recording_saver::TranscriptSegment>, String> {
        self.snapshots
            .iter()
            .find(|(stored_session_id, _)| stored_session_id == session_id)
            .map(|(_, segments)| segments.as_ref().clone())
            .ok_or_else(|| {
                format!("Transcript history is unavailable for recording session {session_id}")
            })
    }
}

static COMPLETED_TRANSCRIPT_HISTORY: Mutex<CompletedTranscriptHistory> =
    Mutex::new(CompletedTranscriptHistory {
        snapshots: VecDeque::new(),
    });
static STOPPING_TRANSCRIPT_SEGMENTS: Mutex<
    Option<(
        String,
        Vec<crate::audio::recording_saver::TranscriptSegment>,
    )>,
> = Mutex::new(None);

fn store_transcript_segment(segment: crate::audio::recording_saver::TranscriptSegment) {
    if let Ok(manager_guard) = RECORDING_MANAGER.lock() {
        if let Some(manager) = manager_guard.as_ref() {
            manager.add_transcript_segment(segment);
            return;
        }
    }

    if let Ok(mut stopping) = STOPPING_TRANSCRIPT_SEGMENTS.lock() {
        if let Some((_, segments)) = stopping.as_mut() {
            if let Some(existing) = segments
                .iter_mut()
                .find(|existing| existing.sequence_id == segment.sequence_id)
            {
                *existing = segment;
            } else {
                segments.push(segment);
            }
        }
    }
}

fn completed_snapshot_after_success(
    terminal_error: &Option<String>,
    segments: Option<Vec<crate::audio::recording_saver::TranscriptSegment>>,
) -> Option<Vec<crate::audio::recording_saver::TranscriptSegment>> {
    terminal_error.is_none().then_some(segments).flatten()
}

// Listener ID for proper cleanup - prevents microphone from staying active after recording stops
static TRANSCRIPT_LISTENER_ID: Mutex<Option<tauri::EventId>> = Mutex::new(None);

// Kept when live transcription starts deferred so the worker can be started if
// the user enables live transcription mid-recording.
static PENDING_TRANSCRIPTION_RECEIVER: Mutex<
    Option<tokio::sync::mpsc::UnboundedReceiver<crate::audio::recording_state::AudioChunk>>,
> = Mutex::new(None);

const TRANSCRIPTION_RUNTIME_START_ERROR_CODE: &str = "TRANSCRIPTION_RUNTIME_INITIALIZATION_FAILED";
const TRANSCRIPTION_RUNTIME_USER_MESSAGE: &str = "Speech recognition could not initialize. Restart Minutes. If the problem continues, repair or reinstall the app.";

// ============================================================================
// PUBLIC TYPES
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RecordingArgs {
    pub save_path: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct TranscriptionStatus {
    pub chunks_in_queue: usize,
    pub is_processing: bool,
    pub last_activity_ms: u64,
}

fn map_recording_start_error<R: Runtime>(app: &AppHandle<R>, error: RecordingStartError) -> String {
    crate::tray::update_tray_menu(app);

    match error {
        RecordingStartError::TranscriptionRuntime(source) => {
            error!("Failed to initialize speech recognition: {source:#}");
            let error = RecordingStartError::TranscriptionRuntime(source);
            if let Err(emit_error) = app.emit(
                "transcription-error",
                serde_json::json!({
                    "error": error.to_string(),
                    "userMessage": TRANSCRIPTION_RUNTIME_USER_MESSAGE,
                    "actionable": false,
                    "phase": "startup"
                }),
            ) {
                error!("Failed to emit transcription runtime startup error: {emit_error}");
            }
            TRANSCRIPTION_RUNTIME_START_ERROR_CODE.to_string()
        }
        RecordingStartError::Other(error) => format!("Failed to start recording: {error}"),
    }
}

// ============================================================================
// DEVICE RESOLUTION
// ============================================================================

/// Resolve the microphone to record with: requested device (if it actually
/// enumerates) → system default → none (system-audio-only recording).
///
/// The device picker has no "no microphone" option: choosing "Default
/// Microphone" sends `None`, so `None` here means "use the system default",
/// NOT "record without a mic". A specifically-requested mic that isn't in
/// cpal's current enumeration (a stale saved device, or a Continuity
/// "iPhone Microphone" that isn't available right now) is downgraded to the
/// system default — the same `default_input_device()` helper the
/// mid-recording disconnect path uses — so start never hard-fails with
/// "Device not found".
///
/// Emits at most one event per call:
/// - `mic-device-switched` — a specific mic was requested but unavailable,
///   and we fell back to the default (reuses the existing frontend listener).
/// - `mic-unavailable` — no usable mic at all; recording proceeds with
///   system audio only. If system audio is also unavailable, start_streams'
///   own guard reports it.
/// Resolving `None` to the default is the user's actual choice, so it's silent.
///
/// ponytail: sync pre-flight substitution (matches Pro), not catch-and-retry —
/// stream.rs keeps its hard-fail as the last line of defense. cpal calls
/// block briefly either way.
fn resolve_mic_or_default<R: Runtime>(
    app: &AppHandle<R>,
    requested_name: Option<&str>,
) -> Option<Arc<super::AudioDevice>> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let requested_specific = requested_name.is_some();

    if let Some(name) = requested_name {
        match parse_audio_device(name) {
            Ok(device) => {
                let exists = cpal::default_host()
                    .input_devices()
                    .map(|mut it| it.any(|d| d.name().map(|n| n == device.name).unwrap_or(false)))
                    .unwrap_or(false);
                if exists {
                    info!("✅ Using requested microphone: '{}'", device.name);
                    return Some(Arc::new(device));
                }
                warn!(
                    "⚠️ Requested mic '{}' not enumerated — falling back to system default",
                    device.name
                );
            }
            Err(e) => {
                warn!(
                    "⚠️ Requested mic '{}' not available: {} — falling back to system default",
                    name, e
                );
            }
        }
    }

    match default_input_device() {
        Ok(device) => {
            info!("✅ Using default microphone: '{}'", device.name);
            if requested_specific {
                // Tell the user their selected mic wasn't available and which
                // mic is actually recording. Reuses the mic-device-switched
                // listener the disconnect path wires up.
                let _ = app.emit(
                    "mic-device-switched",
                    serde_json::json!({ "device_name": device.name }),
                );
            }
            Some(Arc::new(device))
        }
        Err(e) => {
            warn!(
                "❌ No microphone available: {} — recording system audio only",
                e
            );
            let _ = app.emit("mic-unavailable", serde_json::json!({}));
            None
        }
    }
}

/// System-audio analog of `resolve_mic_or_default`: `Some(name)` -> parse it,
/// falling back to the default output if unparseable; `None` ("Default System
/// Audio" in the UI) -> default output. Returns `None` only when no output
/// device exists — system audio is optional, mic-only recording proceeds.
///
/// ponytail: no cpal enumeration check (unlike the mic helper) — Linux system
/// devices are Pulse/ALSA monitor *inputs* tagged Output, so output_devices()
/// would false-negative them. stream.rs still hard-fails on a missing device.
fn resolve_system_or_default(requested_name: Option<&str>) -> Option<Arc<super::AudioDevice>> {
    if let Some(name) = requested_name {
        match parse_audio_device(name) {
            Ok(device) => {
                info!("✅ Using requested system audio: '{}'", device.name);
                return Some(Arc::new(device));
            }
            Err(e) => warn!(
                "⚠️ Requested system audio '{}' not available: {} — falling back to system default",
                name, e
            ),
        }
    }

    match default_output_device() {
        Ok(device) => {
            info!("✅ Using default system audio: '{}'", device.name);
            Some(Arc::new(device))
        }
        Err(e) => {
            warn!(
                "⚠️ No system audio available: {} — recording will continue with microphone only",
                e
            );
            None
        }
    }
}

/// Wake idle audio hardware before checking microphone callbacks, and finish
/// validation before creating any recording resources.
#[cfg(target_os = "macos")]
async fn prepare_audio_for_recording(
    system_device: Option<&super::AudioDevice>,
) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let wake_name = system_device.map(|s| s.name.clone()).or_else(|| {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| d.name().ok())
    });
    if let Some(name) = wake_name.filter(|name| {
        let normalized = name.to_ascii_lowercase();
        !normalized.contains("macbook") && !normalized.contains("built-in")
    }) {
        if let Err(e) = super::recording_manager::wake_audio_connection(&name).await {
            warn!("[AUDIO_WAKE] Wake failed: {} — proceeding anyway", e);
        }
    } else {
        info!("[AUDIO_WAKE] Built-in output selected; skipping unnecessary audio wake");
    }

    if let Err(e) = super::devices::verify_microphone_access().await {
        error!("Microphone access verification failed: {}", e);
        return Err(format!("Microphone access required: {}", e));
    }
    Ok(())
}

// ============================================================================
// RECORDING COMMANDS
// ============================================================================

/// Start recording with default devices
pub async fn start_recording<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    start_recording_with_meeting_name(app, None).await
}

/// Start recording with default devices and optional meeting name
pub async fn start_recording_with_meeting_name<R: Runtime>(
    app: AppHandle<R>,
    meeting_name: Option<String>,
) -> Result<String, String> {
    info!(
        "Starting recording with default devices, meeting: {:?}",
        meeting_name
    );

    cleanup_stopped_capture(&app).await?;
    let _engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    if IS_RECORDING_STOPPING.load(Ordering::SeqCst) {
        return Err("Recording is still stopping".to_string());
    }

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    if super::pipeline::LIVE_TRANSCRIPTION_ENABLED.load(Ordering::SeqCst) {
        if let Err(error) = crate::ensure_onnx_runtime_available() {
            return Err(map_recording_start_error(
                &app,
                RecordingStartError::TranscriptionRuntime(error),
            ));
        }

        info!("🔍 Validating transcription model availability before starting recording...");
        if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await
        {
            error!("Model validation failed: {}", validation_error);
            let _ = app.emit(
                "transcription-error",
                serde_json::json!({
                    "error": validation_error,
                    "userMessage": format!("Recording cannot start: {}", validation_error),
                    "actionable": false,
                    "phase": "startup"
                }),
            );
            return Err(validation_error);
        }
        info!("✅ Transcription model validation passed");
    } else {
        info!("⏺️ Deferred transcription mode: skipping startup model validation");
    }

    // Notify frontend that startup has begun (surfaces STARTING state)
    app.emit(
        "recording-starting",
        serde_json::json!({
            "message": "Recording initialization started"
        }),
    )
    .map_err(|e| e.to_string())?;

    // Load recording preferences to get auto_save AND device preferences
    let (auto_save, preferred_mic_name, preferred_system_name) =
        match super::recording_preferences::load_recording_preferences(&app).await {
            Ok(prefs) => {
                info!("📋 Loaded recording preferences: auto_save={}, preferred_mic={:?}, preferred_system={:?}",
                      prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device);
                (
                    prefs.auto_save,
                    prefs.preferred_mic_device,
                    prefs.preferred_system_device,
                )
            }
            Err(e) => {
                warn!(
                    "Failed to load recording preferences, using defaults: {}",
                    e
                );
                (true, None, None)
            }
        };

    #[cfg(not(target_os = "macos"))]
    let microphone_device = resolve_mic_or_default(&app, preferred_mic_name.as_deref());

    let system_device = resolve_system_or_default(preferred_system_name.as_deref());

    #[cfg(target_os = "macos")]
    prepare_audio_for_recording(system_device.as_deref()).await?;

    #[cfg(target_os = "macos")]
    let microphone_device = resolve_mic_or_default(&app, preferred_mic_name.as_deref());

    // Async-first approach - no more blocking operations!
    info!("🚀 Starting async recording initialization");

    // Create new recording manager only after startup validation succeeds
    let mut manager = RecordingManager::new();
    let session_id = crate::meeting_activity::new_recording_session_id();
    crate::meeting_activity::start_recording(&app, session_id.clone());

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        // Example: Meeting 2025-10-03_08-25-23
        let now = chrono::Local::now();
        format!("Meeting {}", now.format("%Y-%m-%d_%H-%M-%S"))
    });
    manager.set_meeting_name(Some(effective_meeting_name));

    // Set up error callback
    let app_for_error = app.clone();
    let session_for_error = session_id.clone();
    let state_for_error = manager.get_state().clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
        if !state_for_error.is_recording() {
            crate::meeting_activity::fail_recording(
                &app_for_error,
                &session_for_error,
                error.user_message().to_string(),
            );
            let _ = app_for_error.emit(
                "recording-session-error",
                serde_json::json!({
                    "session_id": session_for_error.clone(),
                    "error": error.user_message(),
                }),
            );
        }
    });

    // Start recording with resolved devices (replaces start_recording_with_defaults_and_auto_save call)
    let transcription_receiver = match manager
        .start_recording(microphone_device, system_device, auto_save)
        .await
    {
        Ok(receiver) => receiver,
        Err(error) => {
            let error = map_recording_start_error(&app, error);
            crate::meeting_activity::finish_recording(&app, &session_id, Some(error.clone()));
            return Err(error);
        }
    };
    if !manager.get_state().is_recording() {
        let error = manager
            .get_state()
            .get_last_error()
            .map(|error| error.user_message().to_string())
            .unwrap_or_else(|| "Audio capture stopped during initialization".to_string());
        crate::meeting_activity::finish_recording(&app, &session_id, Some(error.clone()));
        return Err(error);
    }

    // Take the device event receiver BEFORE storing manager globally.
    // A background task will process device events (hot-swap) without frontend polling.
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(RecordingSession {
            manager,
            session_id: session_id.clone(),
        });
    }

    // Spawn background device event processor (mic-disconnect fallback).
    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session.clone());
    }

    // Flip recording live + reset per-session flags (speech-detected latch,
    // mic-recovery budget). Shared with the other start path — see helper.
    finalize_recording_start(&app, session_id.clone());

    let live_transcription_enabled =
        super::pipeline::LIVE_TRANSCRIPTION_ENABLED.load(Ordering::SeqCst);
    if live_transcription_enabled {
        // Start optimized parallel transcription task and store handle
        let task_handle =
            transcription::start_transcription_task(app.clone(), transcription_receiver);
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        *global_task = Some(task_handle);
        drop(global_task);

        // Listen for transcript updates so live history survives page reloads.
        use tauri::Listener;
        let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
            // Parse the transcript update from the event payload
            if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
                // Create structured transcript segment
                let segment = crate::audio::recording_saver::TranscriptSegment {
                    id: format!("seg_{}", update.sequence_id),
                    text: update.text.clone(),
                    audio_start_time: update.audio_start_time,
                    audio_end_time: update.audio_end_time,
                    duration: update.duration,
                    display_time: update.timestamp.clone(), // Use wall-clock timestamp for display
                    confidence: update.confidence,
                    sequence_id: update.sequence_id,
                    speaker: Some(update.source.clone()),
                };

                store_transcript_segment(segment);
            }
        });
        let mut global_listener = TRANSCRIPT_LISTENER_ID.lock().unwrap();
        *global_listener = Some(listener_id);
        info!("✅ Transcript-update event listener registered for history persistence");
    } else {
        // Hold onto the receiver so live transcription can be enabled mid-meeting.
        *PENDING_TRANSCRIPTION_RECEIVER.lock().unwrap() = Some(transcription_receiver);
        *TRANSCRIPTION_TASK.lock().unwrap() = None;
        *TRANSCRIPT_LISTENER_ID.lock().unwrap() = None;
        info!("⏺️ Deferred transcription mode active; recording audio without loading a live transcription engine");
    }

    if !session.is_recording() {
        return Err(session
            .get_last_error()
            .map(|error| error.user_message().to_string())
            .unwrap_or_else(|| "Audio capture stopped during initialization".to_string()));
    }

    // Emit success event
    if let Err(error) = app.emit(
        "recording-started",
        serde_json::json!({
            "message": "Recording started successfully with parallel processing",
            "session_id": session_id,
            "devices": ["Default Microphone", "Default System Audio"],
            "workers": 3
        }),
    ) {
        warn!("Recording started but the recording-started event failed: {error}");
    }

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    info!("✅ Recording started successfully with async-first approach");

    Ok(session_id)
}

/// Start recording with specific devices
pub async fn start_recording_with_devices<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
) -> Result<String, String> {
    start_recording_with_devices_and_meeting(app, mic_device_name, system_device_name, None).await
}

/// Start recording with specific devices and optional meeting name
pub async fn start_recording_with_devices_and_meeting<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
) -> Result<String, String> {
    info!(
        "Starting recording with specific devices: mic={:?}, system={:?}, meeting={:?}",
        mic_device_name, system_device_name, meeting_name
    );

    cleanup_stopped_capture(&app).await?;
    let _engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    if IS_RECORDING_STOPPING.load(Ordering::SeqCst) {
        return Err("Recording is still stopping".to_string());
    }

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    if super::pipeline::LIVE_TRANSCRIPTION_ENABLED.load(Ordering::SeqCst) {
        if let Err(error) = crate::ensure_onnx_runtime_available() {
            return Err(map_recording_start_error(
                &app,
                RecordingStartError::TranscriptionRuntime(error),
            ));
        }

        info!("🔍 Validating transcription model availability before starting recording...");
        if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await
        {
            error!("Model validation failed: {}", validation_error);
            let _ = app.emit(
                "transcription-error",
                serde_json::json!({
                    "error": validation_error,
                    "userMessage": format!("Recording cannot start: {}", validation_error),
                    "actionable": false,
                    "phase": "startup"
                }),
            );
            return Err(validation_error);
        }
        info!("✅ Transcription model validation passed");
    } else {
        info!("⏺️ Deferred transcription mode: skipping startup model validation");
    }

    // Notify frontend that startup has begun (surfaces STARTING state)
    app.emit(
        "recording-starting",
        serde_json::json!({
            "message": "Recording initialization started"
        }),
    )
    .map_err(|e| e.to_string())?;

    #[cfg(not(target_os = "macos"))]
    let mic_device = resolve_mic_or_default(&app, mic_device_name.as_deref());

    let system_device = resolve_system_or_default(system_device_name.as_deref());

    #[cfg(target_os = "macos")]
    prepare_audio_for_recording(system_device.as_deref()).await?;

    #[cfg(target_os = "macos")]
    let mic_device = resolve_mic_or_default(&app, mic_device_name.as_deref());

    // Async-first approach for custom devices - no more blocking operations!
    info!("🚀 Starting async recording initialization with custom devices");

    // Create new recording manager
    let mut manager = RecordingManager::new();
    let session_id = crate::meeting_activity::new_recording_session_id();
    crate::meeting_activity::start_recording(&app, session_id.clone());

    // Load recording preferences to check auto_save setting
    let auto_save = match super::recording_preferences::load_recording_preferences(&app).await {
        Ok(prefs) => {
            info!(
                "📋 Loaded recording preferences: auto_save={}",
                prefs.auto_save
            );
            prefs.auto_save
        }
        Err(e) => {
            warn!(
                "Failed to load recording preferences, defaulting to auto_save=true: {}",
                e
            );
            true // Default to saving if preferences can't be loaded
        }
    };

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        let now = chrono::Local::now();
        format!("Meeting {}", now.format("%Y-%m-%d_%H-%M-%S"))
    });
    manager.set_meeting_name(Some(effective_meeting_name));

    // Set up error callback
    let app_for_error = app.clone();
    let session_for_error = session_id.clone();
    let state_for_error = manager.get_state().clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
        if !state_for_error.is_recording() {
            crate::meeting_activity::fail_recording(
                &app_for_error,
                &session_for_error,
                error.user_message().to_string(),
            );
            let _ = app_for_error.emit(
                "recording-session-error",
                serde_json::json!({
                    "session_id": session_for_error.clone(),
                    "error": error.user_message(),
                }),
            );
        }
    });

    // Start recording with specified devices and auto_save setting
    let transcription_receiver = match manager
        .start_recording(mic_device, system_device, auto_save)
        .await
    {
        Ok(receiver) => receiver,
        Err(error) => {
            let error = map_recording_start_error(&app, error);
            crate::meeting_activity::finish_recording(&app, &session_id, Some(error.clone()));
            return Err(error);
        }
    };
    if !manager.get_state().is_recording() {
        let error = manager
            .get_state()
            .get_last_error()
            .map(|error| error.user_message().to_string())
            .unwrap_or_else(|| "Audio capture stopped during initialization".to_string());
        crate::meeting_activity::finish_recording(&app, &session_id, Some(error.clone()));
        return Err(error);
    }

    // Take the device event receiver BEFORE storing manager globally.
    // A background task will process device events (hot-swap) without frontend polling.
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(RecordingSession {
            manager,
            session_id: session_id.clone(),
        });
    }

    // Spawn background device event processor (mic-disconnect fallback).
    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session.clone());
    }

    // Flip recording live + reset per-session flags (speech-detected latch,
    // mic-recovery budget). Shared with the other start path — see helper.
    finalize_recording_start(&app, session_id.clone());

    let live_transcription_enabled =
        super::pipeline::LIVE_TRANSCRIPTION_ENABLED.load(Ordering::SeqCst);
    if live_transcription_enabled {
        // Start optimized parallel transcription task and store handle
        let task_handle =
            transcription::start_transcription_task(app.clone(), transcription_receiver);
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        *global_task = Some(task_handle);
        drop(global_task);

        // Listen for transcript updates so live history survives page reloads.
        use tauri::Listener;
        let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
            // Parse the transcript update from the event payload
            if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
                // Create structured transcript segment
                let segment = crate::audio::recording_saver::TranscriptSegment {
                    id: format!("seg_{}", update.sequence_id),
                    text: update.text.clone(),
                    audio_start_time: update.audio_start_time,
                    audio_end_time: update.audio_end_time,
                    duration: update.duration,
                    display_time: update.timestamp.clone(), // Use wall-clock timestamp for display
                    confidence: update.confidence,
                    sequence_id: update.sequence_id,
                    speaker: Some(update.source.clone()),
                };

                store_transcript_segment(segment);
            }
        });
        let mut global_listener = TRANSCRIPT_LISTENER_ID.lock().unwrap();
        *global_listener = Some(listener_id);
        info!("✅ Transcript-update event listener registered for history persistence");
    } else {
        // Hold onto the receiver so live transcription can be enabled mid-meeting.
        *PENDING_TRANSCRIPTION_RECEIVER.lock().unwrap() = Some(transcription_receiver);
        *TRANSCRIPTION_TASK.lock().unwrap() = None;
        *TRANSCRIPT_LISTENER_ID.lock().unwrap() = None;
        info!("⏺️ Deferred transcription mode active; recording audio without loading a live transcription engine");
    }

    if !session.is_recording() {
        return Err(session
            .get_last_error()
            .map(|error| error.user_message().to_string())
            .unwrap_or_else(|| "Audio capture stopped during initialization".to_string()));
    }

    // Emit success event
    if let Err(error) = app.emit(
        "recording-started",
        serde_json::json!({
            "message": "Recording started with custom devices and parallel processing",
            "session_id": session_id,
            "devices": [
                mic_device_name.unwrap_or_else(|| "Default Microphone".to_string()),
                system_device_name.unwrap_or_else(|| "Default System Audio".to_string())
            ],
            "workers": 3
        }),
    ) {
        warn!("Recording started but the recording-started event failed: {error}");
    }

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    info!("✅ Recording started with custom devices using async-first approach");

    Ok(session_id)
}

/// Stop recording with optimized graceful shutdown ensuring NO transcript chunks are lost
pub async fn stop_recording<R: Runtime>(
    app: AppHandle<R>,
    args: RecordingArgs,
) -> Result<(), String> {
    stop_recording_session(app, args, None).await
}

async fn stop_recording_session<R: Runtime>(
    app: AppHandle<R>,
    _args: RecordingArgs,
    expected_session_id: Option<String>,
) -> Result<(), String> {
    info!(
        "🛑 Starting optimized recording shutdown - ensuring ALL transcript chunks are preserved"
    );

    let _stopping_guard = StoppingGuard::try_new()?;
    let _engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    // Check if recording is active
    if !IS_RECORDING.load(Ordering::SeqCst) {
        info!("Recording was not active");
        return Ok(());
    }

    let session_id = crate::meeting_activity::recording_identity()
        .map(|recording| recording.session_id)
        .ok_or_else(|| "Active recording has no session identity".to_string())?;
    if !session_matches(expected_session_id.as_deref(), &session_id) {
        return Err("Recording session changed before cleanup".to_string());
    }

    // Emit shutdown progress to frontend
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "stopping_audio",
            "message": "Stopping audio capture...",
            "progress": 20
        }),
    );

    // Step 1: Stop audio capture immediately (no more new chunks) with proper error handling
    *STOPPING_TRANSCRIPT_SEGMENTS.lock().unwrap() = Some((session_id.clone(), Vec::new()));
    let manager_for_cleanup = {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        global_manager.take()
    };

    // Mark the stop tail as in progress so a mic-disconnect fallback that was
    // queued before Stop short-circuits instead of retrying against the taken
    // manager. IS_RECORDING itself stays true until the tail completes — the
    // frontend polls it to keep the stop UI up. RAII so a panic in the tail
    // below can't leave the flag stuck true.
    let stop_result = if let Some(mut manager) = manager_for_cleanup {
        // Use FORCE FLUSH to immediately process all accumulated audio - eliminates 30s delay!
        info!("🚀 Using FORCE FLUSH to eliminate pipeline accumulation delays");
        let result = manager.stop_streams_and_force_flush().await;
        // Keep the stopped manager reachable until the transcription worker drains;
        // transcript-update listeners append the final segments through this slot.
        *RECORDING_MANAGER.lock().unwrap() = Some(manager);
        result
    } else {
        warn!("No recording manager found to stop");
        Ok(())
    };

    let stop_error = match stop_result {
        Ok(_) => {
            info!("✅ Audio streams stopped successfully - no more chunks will be created");
            None
        }
        Err(e) => {
            error!("❌ Failed to stop audio streams: {}", e);
            Some(format!("Failed to stop audio streams: {e}"))
        }
    };

    crate::meeting_activity::set_recording_status(
        &app,
        &session_id,
        crate::meeting_activity::ActivityStatus::Saving,
    );

    // Step 2: Signal transcription workers to finish processing ALL queued chunks
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "processing_transcripts",
            "message": "Processing remaining transcript chunks...",
            "progress": 40
        }),
    );

    // Wait for transcription task with enhanced progress monitoring (NO TIMEOUT - we must process all chunks)
    let transcription_task = {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        global_task.take()
    };
    *PENDING_TRANSCRIPTION_RECEIVER.lock().unwrap() = None;

    let transcription_error = if let Some(mut task_handle) = transcription_task {
        info!("⏳ Waiting for queued transcription chunks to drain");

        // Enhanced progress monitoring during shutdown
        let progress_app = app.clone();
        let progress_task = tokio::spawn(async move {
            let last_update = std::time::Instant::now();

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Emit periodic progress updates during shutdown
                let elapsed = last_update.elapsed().as_secs();
                let _ = progress_app.emit(
                    "recording-shutdown-progress",
                    serde_json::json!({
                        "stage": "processing_transcripts",
                        "message": format!("Processing transcripts... ({}s elapsed)", elapsed),
                        "progress": 40,
                        "detailed": true,
                        "elapsed_seconds": elapsed
                    }),
                );
            }
        });

        // Wait up to 10 minutes for transcription completion to prevent indefinite hangs
        let drain_error = match tokio::time::timeout(
            tokio::time::Duration::from_secs(600), // 10 minutes max
            &mut task_handle,
        )
        .await
        {
            Ok(Ok(())) => {
                info!("✅ ALL transcription chunks processed successfully - no data lost");
                None
            }
            Ok(Err(e)) => {
                warn!("⚠️ Transcription task completed with error: {:?}", e);
                Some(format!(
                    "Transcription worker failed before final history was captured: {e}"
                ))
            }
            Err(_) => {
                warn!("⏱️ Transcription timeout (10 minutes) reached, continuing shutdown to prevent indefinite hang");
                task_handle.abort();
                Some("Timed out while finishing transcription".to_string())
            }
        };

        // Stop progress monitoring
        progress_task.abort();
        drain_error
    } else {
        info!("ℹ️ No transcription task found to wait for");
        None
    };

    // The worker has drained, so no more final transcript events can arrive.
    {
        use tauri::Listener;
        if let Some(listener_id) = TRANSCRIPT_LISTENER_ID.lock().unwrap().take() {
            app.unlisten(listener_id);
            info!("✅ Transcript-update listener removed");
        }
    }

    let manager_for_cleanup = RECORDING_MANAGER.lock().unwrap().take();
    if let Some((stopping_session_id, segments)) =
        STOPPING_TRANSCRIPT_SEGMENTS.lock().unwrap().take()
    {
        if stopping_session_id == session_id {
            if let Some(manager) = manager_for_cleanup.as_ref() {
                for segment in segments {
                    manager.add_transcript_segment(segment);
                }
            }
        } else {
            warn!(
                "Discarding stopped transcript tail for unexpected session {}",
                stopping_session_id
            );
        }
    }

    // Step 3: Now safely unload Whisper model after ALL chunks are processed
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "unloading_model",
            "message": "Unloading speech recognition model...",
            "progress": 70
        }),
    );

    info!("🧠 All transcript chunks processed. Now safely unloading transcription model...");

    // Determine which provider was used and unload the appropriate model (with timeout)
    let config = match tokio::time::timeout(
        tokio::time::Duration::from_secs(30), // 30 seconds max for DB operation
        crate::api::api::api_get_transcript_config(app.clone(), app.clone().state(), None),
    )
    .await
    {
        Ok(Ok(Some(config))) => Some(config.provider),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            warn!("⚠️ Failed to get transcript config: {:?}", e);
            None
        }
        Err(_) => {
            warn!("⏱️ Transcript config timeout (30s), continuing shutdown");
            None
        }
    };

    match config.as_deref() {
        Some("parakeet") => {
            info!("🦜 Unloading Parakeet model...");
            let engine_clone = {
                let engine_guard = crate::parakeet_engine::commands::PARAKEET_ENGINE
                    .lock()
                    .unwrap();
                engine_guard.as_ref().cloned()
            };

            if let Some(engine) = engine_clone {
                let current_model = engine
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                info!("Current Parakeet model before unload: '{}'", current_model);

                if engine.unload_model().await {
                    info!(
                        "✅ Parakeet model '{}' unloaded successfully",
                        current_model
                    );
                } else {
                    warn!("⚠️ Failed to unload Parakeet model '{}'", current_model);
                }
            } else {
                warn!("⚠️ No Parakeet engine found to unload model");
            }
        }
        _ => {
            // Default to Whisper
            info!("🎤 Unloading Whisper model...");
            let engine_clone = {
                let engine_guard = crate::whisper_engine::commands::WHISPER_ENGINE
                    .lock()
                    .unwrap();
                engine_guard.as_ref().cloned()
            };

            if let Some(engine) = engine_clone {
                let current_model = engine
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                info!("Current Whisper model before unload: '{}'", current_model);

                if engine.unload_model().await {
                    info!("✅ Whisper model '{}' unloaded successfully", current_model);
                } else {
                    warn!("⚠️ Failed to unload Whisper model '{}'", current_model);
                }
            } else {
                warn!("⚠️ No Whisper engine found to unload model");
            }
        }
    }

    // Step 3.5: Track meeting ended analytics with privacy-safe metadata
    // Extract all data from manager BEFORE any async operations to avoid Send issues
    let analytics_data = if let Some(ref manager) = manager_for_cleanup {
        let state = manager.get_state();
        let stats = state.get_stats();

        Some((
            manager.get_recording_duration(),
            manager.get_active_recording_duration().unwrap_or(0.0),
            manager.get_total_pause_duration(),
            manager.get_transcript_segments().len() as u64,
            state.has_fatal_error(),
            state.get_microphone_device().map(|d| d.name.clone()),
            state.get_system_device().map(|d| d.name.clone()),
            stats.chunks_processed,
        ))
    } else {
        None
    };

    // Now perform async analytics tracking without holding manager reference
    if let Some((
        total_duration,
        active_duration,
        pause_duration,
        transcript_segments_count,
        had_fatal_error,
        mic_device_name,
        sys_device_name,
        chunks_processed,
    )) = analytics_data
    {
        info!("📊 Collecting analytics for meeting end");

        // Helper function to classify device type from device name (privacy-safe)
        fn classify_device_type(device_name: &str) -> &'static str {
            let name_lower = device_name.to_lowercase();
            // Check for Bluetooth keywords
            if name_lower.contains("bluetooth")
                || name_lower.contains("airpods")
                || name_lower.contains("beats")
                || name_lower.contains("headphones")
                || name_lower.contains("bt ")
                || name_lower.contains("wireless")
            {
                "Bluetooth"
            } else {
                "Wired"
            }
        }

        // Get transcription model info (already loaded above for model unload)
        let transcription_config = match crate::api::api::api_get_transcript_config(
            app.clone(),
            app.clone().state(),
            None,
        )
        .await
        {
            Ok(Some(config)) => Some((config.provider, config.model)),
            _ => None,
        };

        let (transcription_provider, transcription_model) =
            transcription_config.unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

        // Get summary model info from API
        let summary_config =
            match crate::api::api::api_get_model_config(app.clone(), app.clone().state(), None)
                .await
            {
                Ok(Some(config)) => Some((config.provider, config.model)),
                _ => None,
            };

        let (summary_provider, summary_model) =
            summary_config.unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

        // Classify device types (privacy-safe)
        let microphone_device_type = mic_device_name
            .as_ref()
            .map(|name| classify_device_type(name))
            .unwrap_or("Unknown");

        let system_audio_device_type = sys_device_name
            .as_ref()
            .map(|name| classify_device_type(name))
            .unwrap_or("Unknown");

        // Track meeting ended event with privacy-safe data
        match crate::analytics::commands::track_meeting_ended(
            transcription_provider.clone(),
            transcription_model.clone(),
            summary_provider.clone(),
            summary_model.clone(),
            total_duration,
            active_duration,
            pause_duration,
            microphone_device_type.to_string(),
            system_audio_device_type.to_string(),
            chunks_processed,
            transcript_segments_count,
            had_fatal_error,
        )
        .await
        {
            Ok(_) => info!("✅ Analytics tracked successfully for meeting end"),
            Err(e) => warn!("⚠️ Failed to track analytics: {}", e),
        }
    }

    // Step 4: Finalize recording state and cleanup resources safely
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "finalizing",
            "message": "Finalizing recording and cleaning up resources...",
            "progress": 90
        }),
    );

    // Perform final cleanup with the manager if available
    let (meeting_folder, meeting_name, save_error, completed_transcripts) =
        if let Some(mut manager) = manager_for_cleanup {
            info!("🧹 Performing final cleanup and saving recording data");

            // Extract meeting info BEFORE async operations
            let meeting_folder = manager.get_meeting_folder();
            let meeting_name = manager.get_meeting_name();

            let (save_error, completed_transcripts) = match tokio::time::timeout(
                tokio::time::Duration::from_secs(300), // 5 minutes max for file I/O
                manager.save_recording_only(&app),
            )
            .await
            {
                Ok(Ok(completed_transcripts)) => {
                    info!("✅ Recording data saved successfully during cleanup");
                    (None, Some(completed_transcripts))
                }
                Ok(Err(e)) => {
                    warn!(
                        "⚠️ Error during recording cleanup (transcripts preserved): {}",
                        e
                    );
                    (Some(format!("Failed to save recording: {e}")), None)
                }
                Err(_) => {
                    warn!(
                        "⏱️ File I/O timeout (5 minutes) reached during save, continuing shutdown"
                    );
                    (Some("Timed out while saving recording".to_string()), None)
                }
            };

            (
                meeting_folder,
                meeting_name,
                save_error,
                completed_transcripts,
            )
        } else {
            info!("ℹ️ No recording manager available for cleanup");
            (
                None,
                None,
                Some("Recording manager was unavailable during save".to_string()),
                None,
            )
        };

    // Set recording flag to false
    info!("🔍 Setting IS_RECORDING to false");
    IS_RECORDING.store(false, Ordering::SeqCst);
    let capture_error = crate::meeting_activity::recording_identity()
        .filter(|recording| recording.session_id == session_id)
        .and_then(|recording| recording.error);
    let terminal_error = stop_error
        .or(transcription_error)
        .or(save_error)
        .or(capture_error);
    if let Some(segments) = completed_snapshot_after_success(&terminal_error, completed_transcripts)
    {
        COMPLETED_TRANSCRIPT_HISTORY
            .lock()
            .unwrap()
            .publish(session_id.clone(), segments);
    }
    crate::meeting_activity::finish_recording(&app, &session_id, terminal_error.clone());
    // IS_RECORDING_STOPPING is cleared by _stopping_guard on scope exit.

    // Step 4.5: Prepare metadata for frontend (NO database save)
    // NOTE: We do NOT save to database here. The frontend will save after all transcripts are displayed.
    // This ensures the user sees all transcripts streaming in before the database save happens.
    let (folder_path_str, meeting_name_str) = match (&meeting_folder, &meeting_name) {
        (Some(path), Some(name)) => (Some(path.to_string_lossy().to_string()), Some(name.clone())),
        _ => (None, None),
    };

    info!("📤 Preparing recording metadata for frontend save");
    info!("   folder_path: {:?}", folder_path_str);
    info!("   meeting_name: {:?}", meeting_name_str);

    // Database save removed - frontend will handle this after receiving all transcripts
    info!("ℹ️ Skipping database save in Rust - frontend will save after all transcripts received");

    if let Some(error) = terminal_error {
        let _ = app.emit(
            "recording-shutdown-progress",
            serde_json::json!({
                "stage": "failed",
                "message": error,
                "progress": null
            }),
        );
        let _ = app.emit(
            "recording-stopped",
            serde_json::json!({
                "message": "Recording stopped with an error",
                "folder_path": folder_path_str,
                "meeting_name": meeting_name_str,
                "error": error
            }),
        );
        crate::tray::update_tray_menu(&app);
        return Err(error);
    }

    // Step 5: Complete shutdown
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "complete",
            "message": "Recording stopped successfully",
            "progress": 100
        }),
    );

    // Emit final stop event with folder_path and meeting_name for frontend to save
    app.emit(
        "recording-stopped",
        serde_json::json!({
            "message": "Recording stopped - frontend will save after all transcripts received",
            "folder_path": folder_path_str,
            "meeting_name": meeting_name_str
        }),
    )
    .map_err(|e| e.to_string())?;

    // Update tray menu to reflect stopped state
    crate::tray::update_tray_menu(&app);

    info!("🎉 Recording stopped successfully with ZERO transcript chunks lost");
    Ok(())
}

/// Check if recording is active
pub async fn is_recording() -> bool {
    if IS_RECORDING_STOPPING.load(Ordering::SeqCst) {
        return true;
    }
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return false;
    }
    RECORDING_MANAGER
        .lock()
        .unwrap()
        .as_ref()
        .map(|manager| manager.get_state().is_recording())
        .unwrap_or(false)
}

pub fn has_recording_session() -> bool {
    IS_RECORDING.load(Ordering::SeqCst)
}

/// Active recording duration in seconds, excluding pauses.
///
/// Returns `None` when no recording session is active.
pub fn current_active_duration_seconds() -> Option<f64> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    manager_guard
        .as_ref()
        .and_then(|manager| manager.get_active_recording_duration())
}

/// Start the live transcription worker during an active recording.
///
/// Used when the user enables live transcription mid-meeting after starting in
/// deferred mode. The transcription receiver was retained at start for this.
pub async fn ensure_live_transcription_running<R: Runtime>(app: &AppHandle<R>) {
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return;
    }

    {
        let task = TRANSCRIPTION_TASK.lock().unwrap();
        if task
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
        {
            return; // Already running.
        }
    }

    let receiver = PENDING_TRANSCRIPTION_RECEIVER
        .lock()
        .ok()
        .and_then(|mut pending| pending.take());
    let Some(receiver) = receiver else {
        return;
    };

    if let Err(error) = transcription::validate_transcription_model_ready(app).await {
        let _ = app.emit(
            "transcription-error",
            serde_json::json!({
                "error": error,
                "userMessage": "Live transcription could not start. Check your model settings.",
                "actionable": true,
                "phase": "active"
            }),
        );
        // Return the receiver so a later attempt can retry.
        *PENDING_TRANSCRIPTION_RECEIVER.lock().unwrap() = Some(receiver);
        return;
    }

    // Persist live history across reloads (the listener was cleared in deferred mode).
    use tauri::Listener;
    let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
        if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
            let segment = crate::audio::recording_saver::TranscriptSegment {
                id: format!("seg_{}", update.sequence_id),
                text: update.text.clone(),
                audio_start_time: update.audio_start_time,
                audio_end_time: update.audio_end_time,
                duration: update.duration,
                display_time: update.timestamp.clone(),
                confidence: update.confidence,
                sequence_id: update.sequence_id,
                speaker: Some(update.source.clone()),
            };
            store_transcript_segment(segment);
        }
    });
    *TRANSCRIPT_LISTENER_ID.lock().unwrap() = Some(listener_id);

    let handle = transcription::start_transcription_task(app.clone(), receiver);
    *TRANSCRIPTION_TASK.lock().unwrap() = Some(handle);
    info!("▶️ Live transcription enabled mid-recording");
}

/// Get recording statistics
pub async fn get_transcription_status() -> TranscriptionStatus {
    TranscriptionStatus {
        chunks_in_queue: 0,
        is_processing: IS_RECORDING.load(Ordering::SeqCst),
        last_activity_ms: 0,
    }
}

/// Pause the current recording
#[tauri::command]
pub async fn pause_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Pausing recording");

    // Check if currently recording
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and pause it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.pause_recording().map_err(|e| e.to_string())?;
        crate::meeting_activity::set_recording_status(
            &app,
            &crate::meeting_activity::recording_identity()
                .map(|recording| recording.session_id)
                .ok_or_else(|| "Active recording has no session identity".to_string())?,
            crate::meeting_activity::ActivityStatus::Paused,
        );

        // Emit pause event to frontend
        app.emit(
            "recording-paused",
            serde_json::json!({
                "message": "Recording paused"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect paused state
        crate::tray::update_tray_menu(&app);

        info!("Recording paused successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Resume the current recording
#[tauri::command]
pub async fn resume_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Resuming recording");

    // Check if currently recording
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and resume it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.resume_recording().map_err(|e| e.to_string())?;
        crate::meeting_activity::set_recording_status(
            &app,
            &crate::meeting_activity::recording_identity()
                .map(|recording| recording.session_id)
                .ok_or_else(|| "Active recording has no session identity".to_string())?,
            crate::meeting_activity::ActivityStatus::Recording,
        );

        // Emit resume event to frontend
        app.emit(
            "recording-resumed",
            serde_json::json!({
                "message": "Recording resumed"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect resumed state
        crate::tray::update_tray_menu(&app);

        info!("Recording resumed successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Check if recording is currently paused
#[tauri::command]
pub async fn is_recording_paused() -> bool {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.is_paused()
    } else {
        false
    }
}

/// Get detailed recording state
#[tauri::command]
pub async fn get_recording_state() -> serde_json::Value {
    let is_recording = IS_RECORDING.load(Ordering::SeqCst);
    let identity = crate::meeting_activity::recording_identity();
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        serde_json::json!({
            "is_recording": is_recording && manager.get_state().is_recording(),
            "is_paused": manager.is_paused(),
            "is_active": manager.is_active(),
            "session_id": identity.as_ref().map(|recording| &recording.session_id),
            "active_meeting_id": identity.as_ref().and_then(|recording| recording.meeting_id.as_ref()),
            "recording_duration": manager.get_recording_duration(),
            "active_duration": manager.get_active_recording_duration(),
            "total_pause_duration": manager.get_total_pause_duration(),
            "current_pause_duration": manager.get_current_pause_duration()
        })
    } else {
        serde_json::json!({
            "is_recording": is_recording,
            "is_paused": false,
            "is_active": false,
            "session_id": identity.as_ref().map(|recording| &recording.session_id),
            "active_meeting_id": identity.as_ref().and_then(|recording| recording.meeting_id.as_ref()),
            "recording_duration": null,
            "active_duration": null,
            "total_pause_duration": 0.0,
            "current_pause_duration": null
        })
    }
}

/// Get the meeting folder path for the current recording
/// Returns the path if a meeting name was set and folder structure initialized
#[tauri::command]
pub async fn get_meeting_folder_path() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager
            .get_meeting_folder()
            .map(|p| p.to_string_lossy().to_string()))
    } else {
        Ok(None)
    }
}

/// Get accumulated transcript segments from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_transcript_history(
    session_id: Option<String>,
) -> Result<Vec<crate::audio::recording_saver::TranscriptSegment>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(requested_session_id) = session_id {
        if let Some(manager) = manager_guard
            .as_ref()
            .filter(|manager| manager.session_id == requested_session_id)
        {
            return Ok(manager.get_transcript_segments());
        }
        drop(manager_guard);
        return COMPLETED_TRANSCRIPT_HISTORY
            .lock()
            .unwrap()
            .get(&requested_session_id);
    }

    Ok(manager_guard
        .as_ref()
        .map(|manager| manager.get_transcript_segments())
        .unwrap_or_default())
}

/// Get meeting name from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_recording_meeting_name() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_meeting_name())
    } else {
        Ok(None)
    }
}

// ============================================================================
// DEVICE MONITORING COMMANDS (AirPods/Bluetooth disconnect/reconnect support)
// ============================================================================

/// Get information about the active audio output device
/// Used to warn users about Bluetooth playback issues
#[tauri::command]
pub async fn get_active_audio_output() -> Result<super::playback_monitor::AudioOutputInfo, String> {
    super::playback_monitor::get_active_audio_output()
        .await
        .map_err(|e| format!("Failed to get audio output info: {}", e))
}

// ============================================================================
// MIC HOT-SWAP (disconnect recovery)
// ============================================================================

// Guard against concurrent mic hot-swap tasks. Only used by the disconnect
// fallback path (trigger_mic_fallback_to_default) — the "chase the new
// default" auto-swap has been removed.
static MIC_SWAP_IN_PROGRESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// Bounded retry budget for the disconnect fallback (P1 #2). Counts COMPLETED
// failed attempts; MIC_SWAP_IN_PROGRESS still prevents overlapping swaps.
static MIC_FALLBACK_FAILED_ATTEMPTS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
const MAX_MIC_FALLBACK_ATTEMPTS: u32 = 3;

/// Perform mic hot-swap using phased locking — never holds RECORDING_MANAGER during I/O
/// except the brief mic-stream stop in Phase 1.
/// If CPAL hangs during stream creation, only this task blocks; stop flow stays unblocked.
async fn perform_mic_hot_swap_task<R: Runtime>(
    new_device_name: String,
    session: &Arc<super::RecordingState>,
    app: AppHandle<R>,
) -> Result<(), String> {
    info!("[HOT_SWAP] Starting mic hot-swap to '{}'", new_device_name);

    match do_mic_swap(&new_device_name, session).await {
        Ok(()) => {
            info!("[HOT_SWAP] Mic switched to '{}'", new_device_name);
            let _ = app.emit(
                "mic-device-switched",
                serde_json::json!({
                    "device_name": new_device_name
                }),
            );
            Ok(())
        }
        Err(e) => {
            if !session_live(session) {
                return Err(e);
            }
            warn!("[HOT_SWAP] First attempt failed: {} — retrying in 500ms", e);
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            match do_mic_swap(&new_device_name, session).await {
                Ok(()) => {
                    info!("[HOT_SWAP] Mic switched to '{}' on retry", new_device_name);
                    let _ = app.emit(
                        "mic-device-switched",
                        serde_json::json!({
                            "device_name": new_device_name
                        }),
                    );
                    Ok(())
                }
                Err(e) => {
                    error!("[HOT_SWAP] Mic swap failed after retry: {}", e);
                    if session_live(session) {
                        let _ = app.emit(
                            "mic-swap-failed",
                            serde_json::json!({
                                "error": e,
                                "device_name": new_device_name
                            }),
                        );
                    }
                    Err(e)
                }
            }
        }
    }
}

/// Phased mic swap — lock is never held during async I/O.
async fn do_mic_swap(
    device_name: &str,
    session: &Arc<super::RecordingState>,
) -> Result<(), String> {
    // Phase 1: Lock briefly — verify identity, take old stream OUT (no teardown under lock)
    let old_mic = {
        let mut guard = RECORDING_MANAGER.lock().unwrap();
        let manager = guard
            .as_mut()
            .ok_or_else(|| "Recording manager not available".to_string())?;
        if !manager.is_recording() {
            return Err("Recording stopped — aborting mic hot-swap".to_string());
        }
        if !Arc::ptr_eq(manager.get_state(), session) {
            return Err("Session changed before hot-swap — aborting".to_string());
        }
        manager.take_mic_stream_for_swap()
    }; // lock released

    // Tear down the dead mic OUTSIDE the lock — cpal stop()/drop on a
    // disconnected BT device can stall on the CoreAudio HAL lock; doing it
    // under RECORDING_MANAGER would freeze stop_recording (deep-review #2).
    // Non-fatal: the replacement stream is created next regardless, so a
    // teardown error/stall on the already-dead device must not abort the swap.
    if let Some(s) = old_mic {
        if let Err(e) = s.stop() {
            warn!(
                "[HOT_SWAP] Failed to stop old mic stream (proceeding): {}",
                e
            );
        }
    }

    // Phase 2: Async I/O WITHOUT lock — may be slow, that's OK
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Build the AudioDevice directly from the name — the caller
    // (trigger_mic_fallback_to_default) already resolved it via
    // default_input_device(). Skipping list_audio_devices() here avoids a
    // full cpal enumeration on the exact BT-transition hot path where it's
    // known to hang 100+ s (see H2 in PR-175 review). The real device
    // validation happens inside AudioStream::create → get_device_and_config
    // which does a targeted host.input_devices() lookup by name.
    let device_arc = std::sync::Arc::new(super::AudioDevice::new(
        device_name.to_string(),
        super::DeviceType::Input,
    ));

    info!(
        "[HOT_SWAP] Creating new mic stream for '{}' (lock released)",
        device_name
    );
    let new_stream = super::stream::AudioStream::create(
        device_arc.clone(),
        session.clone(),
        super::recording_state::DeviceType::Microphone,
        None,
    )
    .await
    .map_err(|e| format!("Failed to create mic stream: {}", e))?;

    // Resolve the current default output OUTSIDE the lock — a CoreAudio stall here
    // must not block stop_recording (which needs RECORDING_MANAGER). (P1 #1)
    let system_name = default_output_device().ok().map(|d| d.name);

    // Phase 3: Lock briefly — install ONLY if still the same session
    {
        let mut guard = RECORDING_MANAGER.lock().unwrap();
        match guard.as_mut() {
            Some(manager) if Arc::ptr_eq(manager.get_state(), session) => {
                manager.set_mic_stream_after_swap(new_stream, device_arc, system_name);
                info!("[HOT_SWAP] Mic hot-swap to '{}' completed", device_name);
            }
            Some(_) => {
                return Err(
                    "Session changed during hot-swap — discarding stale mic stream".to_string(),
                );
            }
            None => {
                return Err("Recording manager gone during hot-swap".to_string());
            }
        }
    } // lock released

    Ok(())
}

/// Background processor for device monitor events during a recording session.
///
/// The ONLY mid-recording mic switch that is allowed is the fallback from a
/// dead device to the system default, triggered by the device monitor's
/// DeviceDisconnected event. Any other device event is explicitly ignored —
/// recording stays on whatever device was picked at start time until the
/// meeting ends.
///
/// Rationale: auto-swapping to a freshly-connected BT device during recording
/// triggers a reliable hang inside cpal's stream creation on macOS. Locking
/// the device at start eliminates that hang and also makes the recording
/// session predictable.
///
/// The task stops automatically when the receiver is dropped (recording
/// ends / monitor stops).
fn spawn_device_event_processor<R: Runtime>(
    app: AppHandle<R>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<DeviceEvent>,
    session: Arc<super::RecordingState>,
) {
    tokio::spawn(async move {
        info!("[DEVICE_EVENTS] Background event processor started");

        while let Some(event) = receiver.recv().await {
            // Skip if recording has stopped
            if !recording_live() {
                info!(
                    "[DEVICE_EVENTS] Recording stopped — ignoring event: {:?}",
                    event
                );
                continue;
            }

            match event {
                DeviceEvent::DeviceDisconnected {
                    ref device_name,
                    ref device_type,
                } => {
                    info!(
                        "[DEVICE_EVENTS] Device disconnected: '{}' ({:?})",
                        device_name, device_type
                    );
                    // The only automatic mid-recording mic change allowed:
                    // when the active microphone dies, fall back to the
                    // system default input. Triggered after the device
                    // monitor's polling threshold fires.
                    if matches!(device_type, DeviceMonitorType::Microphone) {
                        let name = device_name.clone();
                        let app_clone = app.clone();
                        let session = session.clone();
                        tokio::spawn(async move {
                            trigger_mic_fallback_to_default(app_clone, name, session).await;
                        });
                    }
                }
                DeviceEvent::DeviceReconnected {
                    ref device_name,
                    ref device_type,
                } => {
                    // Per product decision: once we have fallen back to the
                    // built-in mic we stay there for the rest of the meeting.
                    // This is intentional — just log and do nothing.
                    info!("[DEVICE_EVENTS] Device reconnected: '{}' ({:?}) — staying on current mic (fallback is sticky)", device_name, device_type);
                }
                DeviceEvent::DeviceListChanged => {
                    debug!("[DEVICE_EVENTS] Device list changed");
                }
            }
        }
        info!("[DEVICE_EVENTS] Background event processor stopped (channel closed)");
    });
}

/// Disconnect fallback: swap the active mic to the system default input
/// device. Triggered from the background device event processor after the
/// device monitor's polling threshold (3 × 2s) fires `DeviceDisconnected`
/// for the active microphone.
///
/// `disconnected_name` is the device that just died. We keep it to detect
/// the edge case where macOS hasn't yet updated the system default input
/// away from the dead device — we wait and retry in that case rather than
/// swapping back to the same broken device.
///
/// This function takes the MIC_SWAP_IN_PROGRESS guard itself; the caller
/// must NOT already hold it. If a swap is somehow already running this
/// returns immediately.
async fn trigger_mic_fallback_to_default<R: Runtime>(
    app: AppHandle<R>,
    disconnected_name: String,
    session: Arc<super::RecordingState>,
) {
    if !session_live(&session) {
        info!(
            "[MIC_FALLBACK] Not recording — skipping fallback for '{}'",
            disconnected_name
        );
        return;
    }

    if MIC_FALLBACK_FAILED_ATTEMPTS.load(Ordering::SeqCst) >= MAX_MIC_FALLBACK_ATTEMPTS {
        warn!(
            "[MIC_FALLBACK] {} failed attempts reached — giving up on '{}' (terminal event already announced)",
            MAX_MIC_FALLBACK_ATTEMPTS, disconnected_name
        );
        return;
    }

    if MIC_SWAP_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        info!(
            "[MIC_FALLBACK] Swap already in progress — skipping fallback for '{}'",
            disconnected_name
        );
        return;
    }

    // Guard that clears MIC_SWAP_IN_PROGRESS on any return path below so a
    // panic or early return can't leave the flag stuck.
    struct SwapGuard;
    impl Drop for SwapGuard {
        fn drop(&mut self) {
            MIC_SWAP_IN_PROGRESS.store(false, Ordering::SeqCst);
        }
    }
    let _guard = SwapGuard;

    info!(
        "[MIC_FALLBACK] Starting fallback from disconnected device '{}'",
        disconnected_name
    );

    // Let macOS finish swapping the system default input away from the dead
    // device. 150ms is enough in practice for the built-in mic to become the
    // default when an explicitly-selected BT device disconnects.
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    // A Stop-A/Start-B during the sleep swapped our session out. Bail silently —
    // emitting or spending the recovery budget here would fire against B with
    // A's device. Covers the default_input_device() error branch below.
    if !session_live(&session) {
        info!(
            "[MIC_FALLBACK] Session no longer live after wait — aborting fallback for '{}'",
            disconnected_name
        );
        return;
    }

    // Query the current system default input. If it still reports the
    // disconnected device, back off once more and re-query — this handles
    // the edge case where the OS hasn't propagated the change yet.
    let fallback_name = match default_input_device() {
        Ok(dev) => dev.name,
        Err(e) => {
            error!("[MIC_FALLBACK] Failed to query default input device: {}", e);
            let _ = app.emit(
                "mic-swap-failed",
                serde_json::json!({
                    "error": format!("Failed to query default input: {}", e),
                    "device_name": disconnected_name,
                }),
            );
            let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
            if n == MAX_MIC_FALLBACK_ATTEMPTS {
                let _ = app.emit(
                    "mic-recovery-exhausted",
                    serde_json::json!({ "device_name": disconnected_name }),
                );
            }
            return;
        }
    };

    let fallback_name = if fallback_name == disconnected_name {
        warn!(
            "[MIC_FALLBACK] Default input still reports disconnected device '{}' — retrying after 300ms",
            disconnected_name
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        if !session_live(&session) {
            info!("[MIC_FALLBACK] Session no longer live after retry wait — aborting fallback for '{}'", disconnected_name);
            return;
        }
        match default_input_device() {
            Ok(dev) if dev.name != disconnected_name => dev.name,
            Ok(dev) => {
                error!(
                    "[MIC_FALLBACK] Default input still '{}' after retry — aborting fallback",
                    dev.name
                );
                let _ = app.emit(
                    "mic-swap-failed",
                    serde_json::json!({
                        "error": "System default input still reports disconnected device after retry",
                        "device_name": disconnected_name,
                    }),
                );
                let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
                if n == MAX_MIC_FALLBACK_ATTEMPTS {
                    let _ = app.emit(
                        "mic-recovery-exhausted",
                        serde_json::json!({ "device_name": disconnected_name }),
                    );
                }
                return;
            }
            Err(e) => {
                error!(
                    "[MIC_FALLBACK] Failed to re-query default input device: {}",
                    e
                );
                let _ = app.emit(
                    "mic-swap-failed",
                    serde_json::json!({
                        "error": format!("Failed to re-query default input: {}", e),
                        "device_name": disconnected_name,
                    }),
                );
                let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
                if n == MAX_MIC_FALLBACK_ATTEMPTS {
                    let _ = app.emit(
                        "mic-recovery-exhausted",
                        serde_json::json!({ "device_name": disconnected_name }),
                    );
                }
                return;
            }
        }
    } else {
        fallback_name
    };

    info!(
        "[MIC_FALLBACK] Falling back '{}' → '{}'",
        disconnected_name, fallback_name
    );

    // macOS Core Audio pre-wake for the hot-swap path — before we call the
    // rebuild path (which internally calls `AudioDeviceStart` on the new
    // mic), play 150ms of digital silence through the current system
    // output device to force the Core Audio hardware unit out of its idle
    // power state. Without this, `AudioDeviceStart` can return `noErr` but
    // the IO proc will not fire for 10-30 seconds until some other audio
    // nudges the hardware awake — the "backend idle until you play YouTube"
    // symptom from earlier testing.
    //
    // `wake_audio_connection_for_swap` has a built-in fallback: if the
    // current system device name doesn't enumerate (e.g. the BT output just
    // disappeared), it plays through `default_output_device()` instead,
    // which on macOS will now be the built-in speakers — exactly the
    // hardware unit we want to wake for the fallback mic.
    //
    // Non-fatal: on error we log and proceed to the swap anyway. A failed
    // wake is strictly better than no wake.
    #[cfg(target_os = "macos")]
    {
        // Read from the captured session directly (no manager lock) — this
        // stays correct even if the global manager has since been swapped by
        // a Stop/Start of a different session.
        let sys_device_name = session.get_system_device().map(|d| d.name.clone());
        if let Some(name) = sys_device_name {
            match super::recording_manager::wake_audio_connection_for_swap(&name).await {
                Ok(()) => info!("[MIC_FALLBACK] Pre-swap audio wake completed"),
                Err(e) => warn!(
                    "[MIC_FALLBACK] Pre-swap audio wake failed: {} — proceeding anyway",
                    e
                ),
            }
        } else {
            log::debug!("[MIC_FALLBACK] No system device recorded — skipping pre-swap wake");
        }
    }

    // Stop may have started during the sleeps above — bail before touching
    // the (possibly already taken) manager.
    if !session_live(&session) {
        info!(
            "[MIC_FALLBACK] Recording stopping — aborting fallback for '{}'",
            disconnected_name
        );
        return;
    }

    // perform_mic_hot_swap_task performs its own retry-once logic on failure
    // and emits the mic-device-switched / mic-swap-failed events, so we can
    // just delegate here. It does NOT touch MIC_SWAP_IN_PROGRESS internally.
    match perform_mic_hot_swap_task(fallback_name.clone(), &session, app.clone()).await {
        Ok(()) => {
            info!(
                "[MIC_FALLBACK] Fallback complete: now recording via '{}'",
                fallback_name
            );
            MIC_FALLBACK_FAILED_ATTEMPTS.store(0, Ordering::SeqCst);
        }
        Err(e) => {
            error!("[MIC_FALLBACK] Fallback swap failed: {}", e);
            if !session_live(&session) {
                return;
            }
            let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
            if n == MAX_MIC_FALLBACK_ATTEMPTS {
                let _ = app.emit(
                    "mic-recovery-exhausted",
                    serde_json::json!({ "device_name": disconnected_name }),
                );
            }
        }
    }
}
