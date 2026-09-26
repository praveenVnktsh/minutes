// Public provider and engine paths intentionally mirror their implementation filenames.
#![allow(clippy::module_inception)]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex as StdMutex;
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_store::StoreExt;
// Removed unused import

// Performance optimization: Conditional logging macros for hot paths
#[cfg(debug_assertions)]
macro_rules! perf_debug {
    ($($arg:tt)*) => {
        log::debug!($($arg)*)
    };
}

#[cfg(not(debug_assertions))]
macro_rules! perf_debug {
    ($($arg:tt)*) => {};
}

#[cfg(debug_assertions)]
macro_rules! perf_trace {
    ($($arg:tt)*) => {
        log::trace!($($arg)*)
    };
}

#[cfg(not(debug_assertions))]
macro_rules! perf_trace {
    ($($arg:tt)*) => {};
}

/// User-facing product name, used for window titles, notifications, and tray text.
pub const APP_NAME: &str = "Minutes";

// Re-export async logging macros for external use (removed due to macro conflicts)

// Declare audio module
pub mod analytics;
pub mod anthropic;
pub mod api;
pub mod audio;
pub mod calendar;
pub mod config;
pub mod console_utils;
pub mod database;
pub mod debug_mode;
pub mod download_eta;
pub mod groq;
pub mod live_notes;
pub mod meeting_activity;
pub mod meeting_assistant;
pub mod meeting_detection;
pub mod meeting_prompt;
pub mod notifications;
pub mod ollama;
pub mod onboarding;
pub mod openai;
pub mod openrouter;
pub mod parakeet_engine;
pub mod shortcuts;
pub mod state;
pub mod summary;
pub mod tray;
pub mod utils;
pub mod webhooks;
pub mod whisper_engine;

use audio::{list_audio_devices, AudioDevice, PermissionReport};
use log::{error as log_error, info as log_info};
use notifications::commands::NotificationManagerState;
use std::sync::Arc;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::RwLock;

static RECORDING_FLAG: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
static ONNX_RUNTIME_INIT_ERROR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub(crate) fn ensure_onnx_runtime_available() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    if let Some(error) = ONNX_RUNTIME_INIT_ERROR.get() {
        anyhow::bail!("{error}");
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn catch_onnx_runtime_init<F, E>(init: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), E>,
    E: std::fmt::Display,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(init)) {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown panic");
            Err(format!("ONNX Runtime initialization panicked: {message}"))
        }
    }
}

#[cfg(target_os = "windows")]
fn record_onnx_runtime_failure(error: String) {
    log::error!("{error}");
    let _ = ONNX_RUNTIME_INIT_ERROR.set(error);
}

#[cfg(all(test, target_os = "windows"))]
mod onnx_runtime_tests {
    use super::catch_onnx_runtime_init;

    #[test]
    fn catch_onnx_runtime_init_converts_errors_and_panics() {
        assert!(catch_onnx_runtime_init(|| Ok::<(), &str>(())).is_ok());
        assert_eq!(
            catch_onnx_runtime_init(|| Err::<(), _>("initializer error")),
            Err("initializer error".to_string())
        );

        let error = catch_onnx_runtime_init(|| -> Result<(), &str> {
            panic!("synthetic loader panic");
        })
        .unwrap_err();
        assert!(error.contains("synthetic loader panic"));
    }
}

// Global language preference storage (default to "auto-translate" for automatic translation to English)
static LANGUAGE_PREFERENCE: std::sync::LazyLock<StdMutex<String>> =
    std::sync::LazyLock::new(|| StdMutex::new("auto-translate".to_string()));

#[derive(Debug, Deserialize)]
struct RecordingArgs {
    save_path: String,
}

#[derive(Debug, Serialize, Clone)]
struct TranscriptionStatus {
    chunks_in_queue: usize,
    is_processing: bool,
    last_activity_ms: u64,
}

#[derive(Debug, Serialize, Clone)]
struct RecordingStarted {
    session_id: String,
}

fn begin_recording_request(request_id: Option<String>) -> Result<Option<String>, String> {
    match request_id {
        Some(request_id) => {
            meeting_prompt::begin_recording_request(&request_id)?;
            Ok(Some(request_id))
        }
        None => meeting_prompt::claim_legacy_recording_request(),
    }
}

#[tauri::command]
async fn start_recording<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
    request_id: Option<String>,
    resume_meeting_id: Option<String>,
) -> Result<RecordingStarted, String> {
    let request_id = begin_recording_request(request_id)?;
    log_info!("🔥 CALLED start_recording with meeting: {:?}", meeting_name);
    log_info!(
        "📋 Backend received parameters - mic: {:?}, system: {:?}, meeting: {:?}, resume: {:?}",
        mic_device_name,
        system_device_name,
        meeting_name,
        resume_meeting_id
    );

    if is_recording().await {
        let error = "Recording already in progress".to_string();
        if let Some(request_id) = request_id {
            let _ = meeting_prompt::acknowledge_recording_request(
                app,
                request_id,
                false,
                Some(error.clone()),
            );
        }
        return Err(error);
    }

    // Call the actual audio recording system with meeting name
    match audio::recording_commands::start_recording_with_devices_and_meeting(
        app.clone(),
        mic_device_name,
        system_device_name,
        meeting_name.clone(),
        resume_meeting_id,
    )
    .await
    {
        Ok(session_id) => {
            RECORDING_FLAG.store(true, Ordering::SeqCst);
            tray::update_tray_menu(&app);

            if let Some(request_id) = request_id {
                if let Err(error) =
                    meeting_prompt::associate_started_session(&request_id, &session_id)
                {
                    log_error!("Failed to associate recording request: {}", error);
                }
                if let Err(error) = meeting_prompt::acknowledge_recording_request(
                    app.clone(),
                    request_id,
                    true,
                    None,
                ) {
                    log_error!("Failed to acknowledge recording request: {}", error);
                }
            }

            log_info!("Recording started successfully");

            // Show recording started notification through NotificationManager
            // This respects user's notification preferences
            let notification_manager_state = app.state::<NotificationManagerState<R>>();
            if let Err(e) = notifications::commands::show_recording_started_notification(
                &app,
                &notification_manager_state,
                meeting_name.clone(),
            )
            .await
            {
                log_error!("Failed to show recording started notification: {}", e);
            } else {
                log_info!("Successfully showed recording started notification");
            }

            Ok(RecordingStarted { session_id })
        }
        Err(e) => {
            log_error!("Failed to start audio recording: {}", e);
            let error = format!("Failed to start recording: {}", e);
            if let Some(request_id) = request_id {
                let _ = meeting_prompt::acknowledge_recording_request(
                    app,
                    request_id,
                    false,
                    Some(error.clone()),
                );
            }
            Err(error)
        }
    }
}

#[tauri::command]
async fn stop_recording<R: Runtime>(app: AppHandle<R>, args: RecordingArgs) -> Result<(), String> {
    log_info!("Attempting to stop recording...");

    // Check the actual audio recording system state instead of the flag
    if !audio::recording_commands::has_recording_session() {
        log_info!("Recording is already stopped");
        return Ok(());
    }

    // Call the actual audio recording system to stop
    match audio::recording_commands::stop_recording(
        app.clone(),
        audio::recording_commands::RecordingArgs {
            save_path: args.save_path.clone(),
        },
    )
    .await
    {
        Ok(_) => {
            RECORDING_FLAG.store(false, Ordering::SeqCst);
            tray::update_tray_menu(&app);

            // Create the save directory if it doesn't exist
            if let Some(parent) = std::path::Path::new(&args.save_path).parent() {
                if !parent.exists() {
                    log_info!("Creating directory: {:?}", parent);
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        let err_msg = format!("Failed to create save directory: {}", e);
                        log_error!("{}", err_msg);
                        return Err(err_msg);
                    }
                }
            }

            // Show recording stopped notification through NotificationManager
            // This respects user's notification preferences
            let notification_manager_state = app.state::<NotificationManagerState<R>>();
            if let Err(e) = notifications::commands::show_recording_stopped_notification(
                &app,
                &notification_manager_state,
            )
            .await
            {
                log_error!("Failed to show recording stopped notification: {}", e);
            } else {
                log_info!("Successfully showed recording stopped notification");
            }

            Ok(())
        }
        Err(e) => {
            log_error!("Failed to stop audio recording: {}", e);
            // Still update the flag even if stopping failed
            RECORDING_FLAG.store(false, Ordering::SeqCst);
            tray::update_tray_menu(&app);
            Err(format!("Failed to stop recording: {}", e))
        }
    }
}

#[tauri::command]
async fn is_recording() -> bool {
    audio::recording_commands::is_recording().await
}

#[tauri::command]
fn get_transcription_status() -> TranscriptionStatus {
    TranscriptionStatus {
        chunks_in_queue: 0,
        is_processing: false,
        last_activity_ms: 0,
    }
}

#[tauri::command]
fn read_audio_file(file_path: String) -> Result<Vec<u8>, String> {
    match std::fs::read(&file_path) {
        Ok(data) => Ok(data),
        Err(e) => Err(format!("Failed to read audio file: {}", e)),
    }
}

#[tauri::command]
async fn save_transcript(file_path: String, content: String) -> Result<(), String> {
    log_info!("Saving transcript to: {}", file_path);

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&file_path).parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
    }

    // Write content to file
    std::fs::write(&file_path, content)
        .map_err(|e| format!("Failed to write transcript: {}", e))?;

    log_info!("Transcript saved successfully");
    Ok(())
}

// Audio level monitoring commands
#[tauri::command]
async fn start_audio_level_monitoring<R: Runtime>(
    app: AppHandle<R>,
    device_names: Vec<String>,
) -> Result<(), String> {
    log_info!(
        "Starting audio level monitoring for devices: {:?}",
        device_names
    );

    audio::simple_level_monitor::start_monitoring(app, device_names)
        .await
        .map_err(|e| format!("Failed to start audio level monitoring: {}", e))
}

#[tauri::command]
async fn stop_audio_level_monitoring() -> Result<(), String> {
    log_info!("Stopping audio level monitoring");

    audio::simple_level_monitor::stop_monitoring()
        .await
        .map_err(|e| format!("Failed to stop audio level monitoring: {}", e))
}

#[tauri::command]
async fn is_audio_level_monitoring() -> bool {
    audio::simple_level_monitor::is_monitoring()
}

/// Loudness of the audio being recorded right now (RMS of the latest mixed
/// window, 0.0-1.0), polled by the floating record bar to draw its waveform.
#[tauri::command]
fn get_recording_level() -> f32 {
    audio::pipeline::recording_level()
}

// Analytics commands are now handled by analytics::commands module

// Whisper commands are now handled by whisper_engine::commands module

#[tauri::command]
async fn get_audio_devices() -> Result<Vec<AudioDevice>, String> {
    list_audio_devices()
        .await
        .map_err(|e| format!("Failed to list audio devices: {}", e))
}

/// Ask the frontend's onboarding screen whether the microphone is usable, by
/// actually listening for audio rather than by checking whether a stream
/// opened. The verdict is `Authorized` only when a callback carrying a
/// non-zero sample arrived within the wait `verify_microphone` allows —
/// never merely because a stream was constructed. The `Result` here is not
/// load-bearing: `verify_microphone` cannot fail, it answers `Undetermined`
/// instead, so this only keeps the command's existing return shape for the
/// frontend.
#[tauri::command]
async fn trigger_microphone_permission() -> Result<PermissionReport, String> {
    Ok(audio::permission_check::verify_microphone().await)
}

#[tauri::command]
async fn start_recording_with_devices<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
) -> Result<RecordingStarted, String> {
    start_recording_with_devices_and_meeting(
        app,
        mic_device_name,
        system_device_name,
        None,
        None,
        None,
    )
    .await
}

#[tauri::command]
async fn start_recording_with_devices_and_meeting<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
    request_id: Option<String>,
    resume_meeting_id: Option<String>,
) -> Result<RecordingStarted, String> {
    let request_id = begin_recording_request(request_id)?;
    log_info!("🚀 CALLED start_recording_with_devices_and_meeting - Mic: {:?}, System: {:?}, Meeting: {:?}, Resume: {:?}",
             mic_device_name, system_device_name, meeting_name, resume_meeting_id);

    // Clone meeting_name for notification use later
    let meeting_name_for_notification = meeting_name.clone();

    // Call the recording module functions that support meeting names
    let recording_result = match (mic_device_name.clone(), system_device_name.clone()) {
        (None, None) => {
            log_info!(
                "No devices specified, starting with defaults and meeting: {:?}",
                meeting_name
            );
            audio::recording_commands::start_recording_with_meeting_name(
                app.clone(),
                meeting_name,
                resume_meeting_id,
            )
            .await
        }
        _ => {
            log_info!(
                "Starting with specified devices: mic={:?}, system={:?}, meeting={:?}",
                mic_device_name,
                system_device_name,
                meeting_name
            );
            audio::recording_commands::start_recording_with_devices_and_meeting(
                app.clone(),
                mic_device_name,
                system_device_name,
                meeting_name,
                resume_meeting_id,
            )
            .await
        }
    };

    match recording_result {
        Ok(session_id) => {
            log_info!("Recording started successfully via tauri command");

            if let Some(request_id) = request_id {
                if let Err(error) =
                    meeting_prompt::associate_started_session(&request_id, &session_id)
                {
                    log_error!("Failed to associate recording request: {}", error);
                }
                if let Err(error) = meeting_prompt::acknowledge_recording_request(
                    app.clone(),
                    request_id,
                    true,
                    None,
                ) {
                    log_error!("Failed to acknowledge recording request: {}", error);
                }
            }

            // Show recording started notification through NotificationManager
            // This respects user's notification preferences
            let notification_manager_state = app.state::<NotificationManagerState<R>>();
            if let Err(e) = notifications::commands::show_recording_started_notification(
                &app,
                &notification_manager_state,
                meeting_name_for_notification.clone(),
            )
            .await
            {
                log_error!("Failed to show recording started notification: {}", e);
            }

            Ok(RecordingStarted { session_id })
        }
        Err(e) => {
            log_error!("Failed to start recording via tauri command: {}", e);
            if let Some(request_id) = request_id {
                let _ = meeting_prompt::acknowledge_recording_request(
                    app,
                    request_id,
                    false,
                    Some(e.clone()),
                );
            }
            Err(e)
        }
    }
}

#[tauri::command]
async fn set_live_transcription_enabled<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
) -> Result<(), String> {
    log_info!("Setting live transcription enabled: {}", enabled);
    audio::pipeline::LIVE_TRANSCRIPTION_ENABLED.store(enabled, std::sync::atomic::Ordering::SeqCst);
    if enabled {
        // If a recording is already running in deferred mode, start the worker now.
        audio::recording_commands::ensure_live_transcription_running(&app).await;
    }
    Ok(())
}

#[tauri::command]
async fn set_mic_echo_cancellation_enabled(enabled: bool) -> Result<(), String> {
    log_info!("Setting mic echo cancellation (beta) enabled: {}", enabled);
    audio::pipeline::MIC_ECHO_CANCELLATION_ENABLED
        .store(enabled, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
async fn set_language_preference(language: String) -> Result<(), String> {
    let mut lang_pref = LANGUAGE_PREFERENCE
        .lock()
        .map_err(|e| format!("Failed to set language preference: {}", e))?;
    log_info!("Setting language preference to: {}", language);
    *lang_pref = language;
    Ok(())
}

// Internal helper function to get language preference (for use within Rust code)
pub fn get_language_preference_internal() -> Option<String> {
    LANGUAGE_PREFERENCE.lock().ok().map(|lang| lang.clone())
}

/// Parse `minutes://meeting/<id>` (also tolerates `minutes:///meeting/<id>`).
fn meeting_id_from_deep_link(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("minutes://")?;
    let rest = rest.trim_start_matches('/');
    let rest = rest.strip_prefix("meeting/")?;
    let id = rest
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    (!id.is_empty()).then(|| id.to_string())
}

pub fn run() {
    log::set_max_level(log::LevelFilter::Info);

    let mut builder = tauri::Builder::default();

    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            log_info!(
                "Second app instance requested with args: {:?}, cwd: {:?}",
                args,
                cwd
            );

            tray::focus_main_window(app);
        }));
    }

    builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_deep_link::init())
        .manage(whisper_engine::parallel_commands::ParallelProcessorState::new())
        .manage(Arc::new(RwLock::new(
            None::<notifications::manager::NotificationManager<tauri::Wry>>,
        )) as NotificationManagerState<tauri::Wry>)
        .manage(audio::init_system_audio_state())
        .manage(summary::summary_engine::ModelManagerState(Arc::new(
            tokio::sync::Mutex::new(None),
        )))
        .setup(|_app| {
            #[cfg(target_os = "windows")]
            match _app
                .path()
                .resolve("onnxruntime.dll", tauri::path::BaseDirectory::Resource)
            {
                Ok(runtime_path) => {
                    match catch_onnx_runtime_init(|| {
                        ort::init_from(runtime_path.to_string_lossy().into_owned())
                            .with_telemetry(false)
                            .commit()
                            .map(|_| ())
                    }) {
                        Ok(()) => log::info!(
                            "Initialized bundled ONNX Runtime from {}",
                            runtime_path.display()
                        ),
                        Err(error) => record_onnx_runtime_failure(format!(
                            "Failed to initialize bundled ONNX Runtime from {}: {}",
                            runtime_path.display(),
                            error
                        )),
                    }
                }
                Err(error) => record_onnx_runtime_failure(format!(
                    "Failed to resolve bundled ONNX Runtime resource: {}",
                    error
                )),
            };

            log::info!("Application setup complete");

            // Initialize system tray
            if let Err(e) = tray::create_tray(_app.handle()) {
                log::error!("Failed to create system tray: {}", e);
            }

            // Register app-wide shortcuts (toggle recording / window)
            shortcuts::register(_app.handle());

            // Deep links: minutes://meeting/<id> opens that meeting.
            {
                let handle = _app.handle().clone();
                let listener_handle = handle.clone();
                handle.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        if let Some(meeting_id) = meeting_id_from_deep_link(url.as_str()) {
                            tray::open_meeting(&listener_handle, &meeting_id);
                        }
                    }
                });

                // Cold start: the URL may arrive before the listener is attached.
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
                    if let Ok(Some(urls)) = handle.deep_link().get_current() {
                        for url in urls {
                            if let Some(meeting_id) = meeting_id_from_deep_link(url.as_str()) {
                                tray::open_meeting(&handle, &meeting_id);
                                break;
                            }
                        }
                    }
                });
            }

            // Restore debug mode before anything else uses it.
            debug_mode::load(_app.handle());

            // Load the custom transcription vocabulary into the Whisper engine.
            if let Ok(store) = _app.store("store.json") {
                if let Some(value) = store.get("transcriptionVocabulary") {
                    if let Some(vocabulary) = value.as_str() {
                        whisper_engine::whisper_engine::set_transcription_vocabulary(
                            vocabulary.to_string(),
                        );
                    }
                }
            }

            // Initialize notification system with proper defaults
            log::info!("Initializing notification system...");
            let app_for_notif = _app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let notif_state = app_for_notif.state::<NotificationManagerState<tauri::Wry>>();
                match notifications::commands::initialize_notification_manager(
                    app_for_notif.clone(),
                )
                .await
                {
                    Ok(manager) => {
                        // Set default consent and permissions on first launch
                        if let Err(e) = manager.set_consent(true).await {
                            log::error!("Failed to set initial consent: {}", e);
                        }
                        if let Err(e) = manager.request_permission().await {
                            log::error!("Failed to request initial permission: {}", e);
                        }

                        // Store the initialized manager
                        let mut state_lock = notif_state.write().await;
                        *state_lock = Some(manager);
                        log::info!("Notification system initialized with default permissions");
                    }
                    Err(e) => {
                        log::error!("Failed to initialize notification manager: {}", e);
                    }
                }
            });

            // Set models directory to use app_data_dir (unified storage location)
            whisper_engine::commands::set_models_directory(_app.handle());

            // Initialize Whisper engine on startup
            tauri::async_runtime::spawn(async {
                if let Err(e) = whisper_engine::commands::whisper_init().await {
                    log::error!("Failed to initialize Whisper engine on startup: {}", e);
                }
            });

            // Set Parakeet models directory
            parakeet_engine::commands::set_models_directory(_app.handle());

            // Initialize Parakeet engine on startup
            tauri::async_runtime::spawn(async {
                if let Err(e) = parakeet_engine::commands::parakeet_init().await {
                    log::error!("Failed to initialize Parakeet engine on startup: {}", e);
                }
            });

            // Initialize ModelManager for summary engine (async, non-blocking)
            let app_handle_for_model_manager = _app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match summary::summary_engine::commands::init_model_manager_at_startup(
                    &app_handle_for_model_manager,
                )
                .await
                {
                    Ok(_) => log::info!("ModelManager initialized successfully at startup"),
                    Err(e) => {
                        log::warn!("Failed to initialize ModelManager at startup: {}", e);
                        log::warn!("ModelManager will be lazy-initialized on first use");
                    }
                }
            });

            // Initialize database (handles first launch detection and conditional setup)
            tauri::async_runtime::block_on(async {
                database::setup::initialize_database_on_startup(_app.handle()).await
            })
            .expect("Failed to initialize database");

            // Initialize transcription queue worker (processes import/retranscribe tasks sequentially)
            audio::transcription_queue::init_queue_worker(_app.handle());

            // Finish speaker identification that was cut off when the app last quit.
            audio::diarization::init_resume_worker(_app.handle());

            // Deliver durable transcription-complete webhooks in the background.
            webhooks::init_worker(_app.handle());

            // Poll the subscribed calendar feed and raise meeting reminders.
            calendar::init_worker(_app.handle());

            // Initialize bundled templates directory for dynamic template discovery
            log::info!("Initializing bundled templates directory...");
            if let Ok(resource_path) = _app.handle().path().resource_dir() {
                let templates_dir = resource_path.join("templates");
                log::info!(
                    "Setting bundled templates directory to: {:?}",
                    templates_dir
                );
                summary::templates::set_bundled_templates_dir(templates_dir);
            } else {
                log::warn!("Failed to resolve resource directory for templates");
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    tray::hide_main_window(window.app_handle());
                    log::info!("Main window hidden to tray on close request");
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            tray::set_meeting_detected_tray,
            shortcuts::get_global_shortcuts,
            shortcuts::set_global_shortcuts,
            debug_mode::get_debug_mode,
            debug_mode::set_debug_mode,
            debug_mode::get_debug_info,
            debug_mode::delete_debug_meetings,
            meeting_prompt::dismiss_meeting_prompt,
            meeting_prompt::start_recording_from_prompt,
            meeting_prompt::get_pending_recording_request,
            meeting_prompt::claim_recording_request,
            meeting_prompt::acknowledge_recording_request,
            meeting_activity::get_meeting_activity_snapshot,
            meeting_activity::bind_active_recording_meeting,
            start_recording,
            stop_recording,
            is_recording,
            get_transcription_status,
            read_audio_file,
            save_transcript,
            analytics::commands::init_analytics,
            analytics::commands::disable_analytics,
            analytics::commands::track_event,
            analytics::commands::identify_user,
            analytics::commands::track_meeting_started,
            analytics::commands::track_recording_started,
            analytics::commands::track_recording_stopped,
            analytics::commands::track_meeting_deleted,
            analytics::commands::track_settings_changed,
            analytics::commands::track_feature_used,
            analytics::commands::is_analytics_enabled,
            analytics::commands::start_analytics_session,
            analytics::commands::end_analytics_session,
            analytics::commands::track_daily_active_user,
            analytics::commands::track_user_first_launch,
            analytics::commands::is_analytics_session_active,
            analytics::commands::track_summary_generation_started,
            analytics::commands::track_summary_generation_completed,
            analytics::commands::track_summary_regenerated,
            analytics::commands::track_model_changed,
            analytics::commands::track_custom_prompt_used,
            analytics::commands::track_meeting_ended,
            analytics::commands::track_analytics_enabled,
            analytics::commands::track_analytics_disabled,
            analytics::commands::track_analytics_transparency_viewed,
            whisper_engine::commands::whisper_init,
            whisper_engine::commands::whisper_get_available_models,
            whisper_engine::commands::whisper_load_model,
            whisper_engine::commands::whisper_get_current_model,
            whisper_engine::commands::whisper_is_model_loaded,
            whisper_engine::commands::whisper_has_available_models,
            whisper_engine::commands::whisper_validate_model_ready,
            whisper_engine::commands::whisper_transcribe_audio,
            whisper_engine::commands::whisper_get_models_directory,
            whisper_engine::commands::whisper_download_model,
            whisper_engine::commands::whisper_cancel_download,
            whisper_engine::commands::whisper_delete_corrupted_model,
            // Parakeet engine commands
            parakeet_engine::commands::parakeet_init,
            parakeet_engine::commands::parakeet_get_available_models,
            parakeet_engine::commands::parakeet_load_model,
            parakeet_engine::commands::parakeet_get_current_model,
            parakeet_engine::commands::parakeet_is_model_loaded,
            parakeet_engine::commands::parakeet_has_available_models,
            parakeet_engine::commands::parakeet_validate_model_ready,
            parakeet_engine::commands::parakeet_transcribe_audio,
            parakeet_engine::commands::parakeet_get_models_directory,
            parakeet_engine::commands::parakeet_download_model,
            parakeet_engine::commands::parakeet_retry_download,
            parakeet_engine::commands::parakeet_cancel_download,
            parakeet_engine::commands::parakeet_delete_corrupted_model,
            parakeet_engine::commands::open_parakeet_models_folder,
            // Parallel processing commands
            whisper_engine::parallel_commands::initialize_parallel_processor,
            whisper_engine::parallel_commands::start_parallel_processing,
            whisper_engine::parallel_commands::pause_parallel_processing,
            whisper_engine::parallel_commands::resume_parallel_processing,
            whisper_engine::parallel_commands::stop_parallel_processing,
            whisper_engine::parallel_commands::get_parallel_processing_status,
            whisper_engine::parallel_commands::get_system_resources,
            whisper_engine::parallel_commands::check_resource_constraints,
            whisper_engine::parallel_commands::calculate_optimal_workers,
            whisper_engine::parallel_commands::prepare_audio_chunks,
            whisper_engine::parallel_commands::test_parallel_processing_setup,
            get_audio_devices,
            trigger_microphone_permission,
            start_recording_with_devices,
            start_recording_with_devices_and_meeting,
            start_audio_level_monitoring,
            stop_audio_level_monitoring,
            is_audio_level_monitoring,
            get_recording_level,
            // Onboarding microphone check
            audio::mic_check::mic_check_start,
            audio::mic_check::mic_check_cancel,
            // Recording pause/resume commands
            audio::recording_commands::pause_recording,
            audio::recording_commands::resume_recording,
            audio::recording_commands::is_recording_paused,
            audio::recording_commands::get_recording_state,
            audio::recording_commands::get_meeting_folder_path,
            live_notes::save_live_notes,
            live_notes::load_live_notes,
            live_notes::attach_live_notes,
            live_notes::get_meeting_live_notes,
            live_notes::save_meeting_live_notes,
            meeting_assistant::get_meeting_chat,
            meeting_assistant::chat_with_meeting,
            // Reload sync commands (retrieve transcript history and meeting name)
            audio::recording_commands::get_transcript_history,
            audio::recording_commands::get_recording_meeting_name,
            // Playback device detection (Bluetooth warning)
            audio::recording_commands::get_active_audio_output,
            // Audio recovery commands (for transcript recovery feature)
            audio::incremental_saver::recover_audio_from_checkpoints,
            audio::incremental_saver::cleanup_checkpoints,
            audio::incremental_saver::has_audio_checkpoints,
            console_utils::show_console,
            console_utils::hide_console,
            console_utils::toggle_console,
            ollama::get_ollama_models,
            ollama::pull_ollama_model,
            ollama::delete_ollama_model,
            ollama::get_ollama_model_context,
            openai::openai::get_openai_models,
            anthropic::anthropic::get_anthropic_models,
            groq::groq::get_groq_models,
            api::api_get_meetings,
            api::api_set_meeting_pinned,
            api::api_set_meeting_archived,
            api::api_set_meeting_debug,
            api::save_text_export,
            api::api_discard_meeting,
            api::api_get_meeting_audio_path,
            api::api_get_transcription_vocabulary,
            api::api_set_transcription_vocabulary,
            api::api_search_transcripts,
            api::api_get_profile,
            api::api_save_profile,
            api::api_update_profile,
            api::api_get_model_config,
            api::api_save_model_config,
            api::api_get_api_key,
            // api::api_get_auto_generate_setting,
            // api::api_save_auto_generate_setting,
            api::api_get_transcript_config,
            api::api_save_transcript_config,
            api::api_get_transcript_api_key,
            api::api_delete_meeting,
            api::api_get_meeting,
            api::api_get_meeting_metadata,
            api::api_get_meeting_transcripts,
            api::api_save_meeting_title,
            api::api_create_meeting,
            api::api_save_transcript,
            api::open_meeting_folder,
            api::test_backend_connection,
            api::debug_backend_connection,
            api::open_external_url,
            api::feedback_issues_are_open,
            // Custom OpenAI commands
            api::api_save_custom_openai_config,
            api::api_get_custom_openai_config,
            api::api_test_custom_openai_connection,
            // Summary commands
            summary::commands::api_process_transcript,
            summary::commands::api_get_summary,
            summary::commands::api_save_meeting_summary,
            summary::commands::api_get_meeting_summary_language,
            summary::commands::api_save_meeting_summary_language,
            summary::commands::api_get_meeting_detected_summary_language,
            summary::commands::api_save_meeting_detected_summary_language,
            summary::commands::api_detect_transcript_summary_language,
            summary::commands::api_cancel_summary,
            // Template commands
            summary::template_commands::api_list_templates,
            summary::template_commands::api_get_template_details,
            summary::template_commands::api_validate_template,
            // Built-in AI commands
            summary::summary_engine::commands::builtin_ai_list_models,
            summary::summary_engine::commands::builtin_ai_get_model_info,
            summary::summary_engine::commands::builtin_ai_download_model,
            summary::summary_engine::commands::builtin_ai_cancel_download,
            summary::summary_engine::commands::builtin_ai_delete_model,
            summary::summary_engine::commands::builtin_ai_is_model_ready,
            summary::summary_engine::commands::builtin_ai_get_available_summary_model,
            summary::summary_engine::commands::builtin_ai_get_recommended_model,
            openrouter::get_openrouter_models,
            audio::recording_preferences::get_recording_preferences,
            audio::recording_preferences::set_recording_preferences,
            audio::recording_preferences::get_default_recordings_folder_path,
            audio::recording_preferences::open_recordings_folder,
            audio::recording_preferences::select_recording_folder,
            audio::recording_preferences::get_available_audio_backends,
            audio::recording_preferences::get_current_audio_backend,
            audio::recording_preferences::set_audio_backend,
            audio::recording_preferences::get_audio_backend_info,
            // Language preference commands
            set_language_preference,
            // Live transcription toggle
            set_live_transcription_enabled,
            set_mic_echo_cancellation_enabled,
            // Notification system commands
            notifications::commands::get_notification_settings,
            notifications::commands::set_notification_settings,
            notifications::commands::request_notification_permission,
            notifications::commands::show_notification,
            notifications::commands::show_test_notification,
            notifications::commands::is_dnd_active,
            notifications::commands::get_system_dnd_status,
            notifications::commands::set_manual_dnd,
            notifications::commands::set_notification_consent,
            notifications::commands::clear_notifications,
            notifications::commands::is_notification_system_ready,
            notifications::commands::initialize_notification_manager_manual,
            notifications::commands::test_notification_with_auto_consent,
            notifications::commands::get_notification_stats,
            // System audio capture commands
            audio::system_audio_commands::start_system_audio_capture_command,
            audio::system_audio_commands::list_system_audio_devices_command,
            audio::system_audio_commands::check_system_audio_permissions_command,
            audio::system_audio_commands::start_system_audio_monitoring,
            audio::system_audio_commands::stop_system_audio_monitoring,
            audio::system_audio_commands::get_system_audio_monitoring_status,
            // Screen Recording permission commands
            audio::permissions::check_screen_recording_permission_command,
            audio::permissions::request_screen_recording_permission_command,
            audio::permissions::trigger_system_audio_permission_command,
            // Database import commands
            database::commands::check_first_launch,
            database::commands::select_legacy_database_path,
            database::commands::detect_legacy_database,
            database::commands::check_default_legacy_database,
            database::commands::check_homebrew_database,
            database::commands::import_and_initialize_database,
            database::commands::initialize_fresh_database,
            // Database and Models path commands
            database::commands::get_database_directory,
            database::commands::open_database_folder,
            whisper_engine::commands::open_models_folder,
            // Onboarding commands
            onboarding::get_onboarding_status,
            onboarding::save_onboarding_status_cmd,
            onboarding::reset_onboarding_status_cmd,
            onboarding::complete_onboarding,
            // System settings commands
            #[cfg(target_os = "macos")]
            utils::open_system_settings,
            // Retranscription commands
            audio::retranscription::start_retranscription_command,
            audio::retranscription::cancel_retranscription_command,
            audio::retranscription::is_retranscription_in_progress_command,
            audio::diarization::run_speaker_diarization,
            audio::diarization::get_diarization_status,
            audio::speaker_corrections::get_speaker_identities,
            audio::speaker_corrections::rename_speaker,
            audio::speaker_corrections::merge_speakers,
            audio::speaker_corrections::reassign_transcript_speaker,
            // Model provenance (which models transcribed, diarized and summarized a meeting)
            audio::model_provenance::get_meeting_model_provenance,
            // Import audio commands
            audio::import::select_and_validate_audio_command,
            audio::import::validate_audio_file_command,
            audio::import::start_import_audio_command,
            audio::import::cancel_import_command,
            audio::import::is_import_in_progress_command,
            // Transcription queue commands
            audio::transcription_queue::get_transcription_queue_status,
            audio::transcription_queue::cancel_transcription_task,
            audio::transcription_queue::pause_transcription_task,
            audio::transcription_queue::resume_transcription_task,
            audio::transcription_queue::is_transcription_queue_active,
            // Transcription-complete webhook integration
            webhooks::get_webhook_config,
            webhooks::set_webhook_config,
            webhooks::test_webhook,
            // Calendar subscription (ICS/iCal) integration
            calendar::get_calendar_config,
            calendar::set_calendar_config,
            calendar::test_calendar_feed,
            calendar::refresh_calendar_now,
            calendar::get_calendar_events,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            match event {
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen { .. } => {
                    tray::focus_main_window(_app_handle);
                }
                tauri::RunEvent::Exit => {
                    log::info!("Application exiting, cleaning up resources...");
                    tauri::async_runtime::block_on(async {
                        // Clean up database connection and checkpoint WAL
                        if let Some(app_state) = _app_handle.try_state::<state::AppState>() {
                            log::info!("Starting database cleanup...");
                            if let Err(e) = app_state.db_manager.cleanup().await {
                                log::error!("Failed to cleanup database: {}", e);
                            } else {
                                log::info!("Database cleanup completed successfully");
                            }
                        } else {
                            log::warn!(
                                "AppState not available for database cleanup (likely first launch)"
                            );
                        }

                        // Clean up sidecar
                        log::info!("Cleaning up sidecar...");
                        if let Err(e) = summary::summary_engine::force_shutdown_sidecar().await {
                            log::error!("Failed to force shutdown sidecar: {}", e);
                        }
                    });
                    log::info!("Application cleanup complete");
                }
                _ => {}
            }
        });
}
