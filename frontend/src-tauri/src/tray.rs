use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Runtime,
};

/// How many meetings to surface in the tray's "Recent Meetings" submenu.
const RECENT_MEETINGS_LIMIT: usize = 5;

/// Last loaded recent meetings, so transient menu rebuilds (pause/resume) keep
/// showing the list instead of momentarily emptying it.
static RECENT_MEETINGS_CACHE: std::sync::Mutex<Vec<(String, String)>> =
    std::sync::Mutex::new(Vec::new());

fn cached_recent_meetings() -> Vec<(String, String)> {
    RECENT_MEETINGS_CACHE
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

/// App name of a detected meeting awaiting the user's decision, if any.
static MEETING_DETECTED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn detected_meeting() -> Option<String> {
    MEETING_DETECTED.lock().ok().and_then(|guard| guard.clone())
}

/// Set or clear the "meeting detected" tray state.
pub fn set_meeting_detected<R: Runtime>(app: &AppHandle<R>, app_name: Option<String>) {
    if let Ok(mut guard) = MEETING_DETECTED.lock() {
        *guard = app_name;
    }
    update_tray_menu(app);
}

/// Tauri command so the frontend detection can drive the tray badge/menu.
#[tauri::command]
pub async fn set_meeting_detected_tray<R: Runtime>(app: AppHandle<R>, app_name: Option<String>) {
    set_meeting_detected(&app, app_name);
}

/// Format an elapsed duration as `M:SS`, or `H:MM:SS` once it passes an hour.
pub fn format_elapsed(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Build a tray label like `Sep 11 · Weekly planning` for a recent meeting.
fn recent_meeting_label(title: &str, created_at: chrono::DateTime<chrono::Utc>) -> String {
    let when = created_at
        .with_timezone(&chrono::Local)
        .format("%b %-d")
        .to_string();
    format!("{} · {}", when, truncate_title(title, 40))
}

/// Shorten a meeting title for display in the tray menu.
fn truncate_title(title: &str, max_chars: usize) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return "Untitled meeting".to_string();
    }

    let mut chars = trimmed.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Stopped,
    Starting,
    Recording,
    Pausing,
    Paused,
    Resuming,
    Stopping,
}

pub fn create_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Start with default menu, will update with actual state after initialization
    // Pass can_record=true initially, will be updated by update_tray_menu immediately
    let menu = build_menu(app, RecordingState::Stopped, true)?;

    let (icon, icon_as_template) = tray_icon(RecordingState::Stopped, false);
    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip(tray_tooltip(RecordingState::Stopped))
        .icon(icon)
        .icon_as_template(icon_as_template)
        .on_menu_event(|app, event| handle_menu_event(app, event.id.as_ref()))
        .build(app)?;

    // Update tray menu with actual recording state after creation
    update_tray_menu(app);

    // Reflect the live recording duration in the menu bar / tooltip
    start_tray_timer(app);

    Ok(())
}

/// Keep the tray's menu bar title and tooltip in sync with the recording timer.
///
/// `set_title` only renders where the platform supports menu-bar text, but the
/// tooltip update works everywhere. The task idles quietly while not recording.
pub fn start_tray_timer<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut timer_visible = false;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let Some(tray) = app.tray_by_id("main-tray") else {
                continue;
            };

            if crate::audio::recording_commands::is_recording().await {
                // A detected meeting is moot once recording starts.
                if detected_meeting().is_some() {
                    set_meeting_detected(&app, None);
                }
                let Some(seconds) =
                    crate::audio::recording_commands::current_active_duration_seconds()
                else {
                    continue;
                };
                let elapsed = format_elapsed(seconds as u64);
                let paused = crate::audio::recording_commands::is_recording_paused().await;
                let state_text = if paused { "paused" } else { "recording" };

                let _ = tray.set_title(Some(elapsed.clone()));
                let _ = tray.set_tooltip(Some(format!(
                    "{} — {} ({})",
                    crate::APP_NAME,
                    state_text,
                    elapsed
                )));
                set_recording_window_indicators(&app, true, paused, Some(&elapsed));
                timer_visible = true;
            } else {
                if timer_visible {
                    set_recording_window_indicators(&app, false, false, None);
                    timer_visible = false;
                }
                // Idle: the badge on the icon signals detection; keep the menu bar text clear.
                let _ = tray.set_title(None::<&str>);
                let _ = tray.set_tooltip(Some(match detected_meeting() {
                    Some(name) => format!("{} — meeting detected: {}", crate::APP_NAME, name),
                    None => tray_tooltip(RecordingState::Stopped).to_string(),
                }));
            }
        }
    });
}

/// Reflect the recording state in the main window title and dock badge.
fn set_recording_window_indicators<R: Runtime>(
    app: &AppHandle<R>,
    recording: bool,
    paused: bool,
    elapsed: Option<&str>,
) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    if recording {
        let title = if paused {
            format!("Paused — {}", crate::APP_NAME)
        } else {
            match elapsed {
                Some(elapsed) => format!("● Recording {} — {}", elapsed, crate::APP_NAME),
                None => format!("● Recording — {}", crate::APP_NAME),
            }
        };
        let _ = window.set_title(&title);

        // Dock badges are macOS-only in Tauri.
        #[cfg(target_os = "macos")]
        {
            let _ = window.set_badge_label(Some("REC".to_string()));
        }
    } else {
        let _ = window.set_title(crate::APP_NAME);

        #[cfg(target_os = "macos")]
        {
            let _ = window.set_badge_label(None);
        }
    }
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, item_id: &str) {
    if let Some(meeting_id) = item_id.strip_prefix("recent_meeting:") {
        open_meeting(app, meeting_id);
        return;
    }

    match item_id {
        "start_detected_recording" => {
            set_meeting_detected(app, None);
            toggle_recording_handler(app);
        }
        "dismiss_detected" => set_meeting_detected(app, None),
        "toggle_recording" => toggle_recording_handler(app),
        "pause_recording" => pause_recording_handler(app),
        "resume_recording" => resume_recording_handler(app),
        "stop_recording" => stop_recording_handler(app),
        "open_window" => focus_main_window(app),
        "settings" => {
            focus_main_window(app);
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("window.location.assign('/settings')");
            }
        }
        "check_updates" => check_updates_handler(app),
        "quit" => app.exit(0),
        _ => {}
    }
}
/// Open a meeting in the main window (tray recent list, deep links).
pub(crate) fn open_meeting<R: Runtime>(app: &AppHandle<R>, meeting_id: &str) {
    focus_main_window(app);

    let Some(window) = app.get_webview_window("main") else {
        log::warn!("Tray: main window unavailable to open recent meeting");
        return;
    };

    // Encode the id defensively rather than trusting it to be URL-safe.
    let id_literal = serde_json::to_string(meeting_id).unwrap_or_else(|_| "\"\"".to_string());
    let script = format!(
        "window.location.assign('/meeting-details?id=' + encodeURIComponent({id_literal}))"
    );

    if let Err(error) = window.eval(script) {
        log::error!("Tray: failed to open recent meeting: {}", error);
    }
}

pub(crate) fn toggle_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    focus_main_window(app);
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        if crate::is_recording().await {
            // Immediately show stopping state
            set_tray_state(&app_clone, RecordingState::Stopping);

            log::info!("Tray toggle: Stopping recording...");

            // Generate save path (same as RecordingControls.tsx)
            let data_dir = match app_clone.path().app_data_dir() {
                Ok(dir) => dir,
                Err(e) => {
                    log::error!("Failed to get app data dir: {}", e);
                    update_tray_menu_async(&app_clone).await;
                    return;
                }
            };

            let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
            let save_path = data_dir.join(format!("recording-{}.wav", timestamp));

            // Call Rust stop_recording command (like pause/resume pattern)
            let stop_result = crate::audio::recording_commands::stop_recording(
                app_clone.clone(),
                crate::audio::recording_commands::RecordingArgs {
                    save_path: save_path.to_string_lossy().to_string(),
                },
            )
            .await;

            // Handle result
            match stop_result {
                Ok(_) => {
                    log::info!("Tray toggle: Recording stopped successfully");

                    // Trigger frontend post-processing via event (works from any page)
                    // (SQLite save, navigation, analytics)
                    if let Err(e) = app_clone.emit("recording-stop-complete", true) {
                        log::error!(
                            "Tray toggle: Failed to emit recording-stop-complete event: {}",
                            e
                        );
                    }
                }
                Err(e) => {
                    log::error!("Tray toggle: Failed to stop recording: {}", e);
                    // Revert tray state on error
                    update_tray_menu_async(&app_clone).await;
                }
            }
        } else {
            // Immediately show starting state
            set_tray_state(&app_clone, RecordingState::Starting);

            log::info!("Emitting correlated start recording request from tray");
            crate::meeting_prompt::request_recording(&app_clone, "tray");
            if let Some(window) = app_clone.get_webview_window("main") {
                let _ = window.eval("window.location.assign('/')");
            }
        }
    });
}

fn pause_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    // Immediately show pausing state
    set_tray_state(app, RecordingState::Pausing);

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = crate::audio::recording_commands::pause_recording(app_clone.clone()).await {
            log::error!("Failed to pause recording from tray: {}", e);
            // Revert to current state on error
            update_tray_menu_async(&app_clone).await;
        } else {
            log::info!("Recording paused from tray");
            // The pause_recording function will call update_tray_menu, so no need to call it here
        }
    });
}

fn resume_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    // Immediately show resuming state
    set_tray_state(app, RecordingState::Resuming);

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = crate::audio::recording_commands::resume_recording(app_clone.clone()).await
        {
            log::error!("Failed to resume recording from tray: {}", e);
            // Revert to current state on error
            update_tray_menu_async(&app_clone).await;
        } else {
            log::info!("Recording resumed from tray");
            // The resume_recording function will call update_tray_menu, so no need to call it here
        }
    });
}

fn stop_recording_handler<R: Runtime>(app: &AppHandle<R>) {
    // Immediately show stopping state
    set_tray_state(app, RecordingState::Stopping);

    focus_main_window(app);
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        log::info!("Tray: Stopping recording...");

        // Generate save path (same as RecordingControls.tsx)
        let data_dir = match app_clone.path().app_data_dir() {
            Ok(dir) => dir,
            Err(e) => {
                log::error!("Failed to get app data dir: {}", e);
                update_tray_menu_async(&app_clone).await;
                return;
            }
        };

        let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let save_path = data_dir.join(format!("recording-{}.wav", timestamp));

        // Call Rust stop_recording command (like pause/resume pattern)
        let stop_result = crate::audio::recording_commands::stop_recording(
            app_clone.clone(),
            crate::audio::recording_commands::RecordingArgs {
                save_path: save_path.to_string_lossy().to_string(),
            },
        )
        .await;

        // Handle result
        match stop_result {
            Ok(_) => {
                log::info!("Tray: Recording stopped successfully");

                // Trigger frontend post-processing via event (works from any page)
                // (SQLite save, navigation, analytics)
                if let Err(e) = app_clone.emit("recording-stop-complete", true) {
                    log::error!("Tray: Failed to emit recording-stop-complete event: {}", e);
                }
            }
            Err(e) => {
                log::error!("Tray: Failed to stop recording: {}", e);
                // Revert tray state on error
                update_tray_menu_async(&app_clone).await;
            }
        }
    });
}

fn check_updates_handler<R: Runtime>(app: &AppHandle<R>) {
    focus_main_window(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval("window.dispatchEvent(new CustomEvent('check-updates-from-tray'))");
    }
}

pub fn update_tray_menu<R: Runtime>(app: &AppHandle<R>) {
    // For sync update, spawn async task to get current state
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        // Small delay to ensure recording state has been updated
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        update_tray_menu_async(&app_clone).await;
    });
}

pub fn set_tray_state<R: Runtime>(app: &AppHandle<R>, state: RecordingState) {
    log::info!("Tray: Setting intermediate state: {:?}", state);
    // During recording state transitions, we assume recording is allowed (we're already recording)
    if let Ok(menu) = build_menu(app, state, true) {
        if let Some(tray) = app.tray_by_id("main-tray") {
            let result = tray.set_menu(Some(menu));
            log::info!("Tray: Intermediate state menu update result: {:?}", result);
            update_tray_status(&tray, state);
        } else {
            log::warn!("Tray: Could not find tray with id 'main-tray'");
        }
    } else {
        log::error!("Tray: Failed to build menu for intermediate state");
    }
}

async fn get_current_recording_state() -> RecordingState {
    // Check if currently recording
    let is_recording = crate::audio::recording_commands::is_recording().await;
    log::info!(
        "Tray: get_current_recording_state - is_recording: {}",
        is_recording
    );

    if !is_recording {
        log::info!("Tray: Recording state is Stopped");
        return RecordingState::Stopped;
    }

    // Check if paused
    let is_paused = crate::audio::recording_commands::is_recording_paused().await;
    log::info!("Tray: is_paused: {}", is_paused);

    if is_paused {
        log::info!("Tray: Recording state is Paused");
        RecordingState::Paused
    } else {
        log::info!("Tray: Recording state is Recording");
        RecordingState::Recording
    }
}

/// Check if recording is allowed based on onboarding status and transcription model availability
/// Returns true if:
/// - Onboarding is complete (user may prefer Whisper later), OR
/// - Parakeet transcription model is ready (downloaded)
async fn check_can_record<R: Runtime>(app: &AppHandle<R>) -> bool {
    // First check if onboarding is complete
    let onboarding_complete = match crate::onboarding::load_onboarding_status(app).await {
        Ok(status) => status.completed,
        Err(e) => {
            log::warn!(
                "Tray: Failed to load onboarding status: {}, assuming complete",
                e
            );
            true // Assume complete if we can't check (safe default)
        }
    };

    // If onboarding is complete, always allow recording
    // (user may prefer Whisper or have their own transcription setup)
    if onboarding_complete {
        return true;
    }

    // During onboarding, check if Parakeet transcription model is ready
    match crate::parakeet_engine::commands::parakeet_has_available_models().await {
        Ok(has_models) => has_models,
        Err(e) => {
            log::warn!(
                "Tray: Failed to check Parakeet models: {}, assuming not ready",
                e
            );
            false
        }
    }
}

pub async fn update_tray_menu_async<R: Runtime>(app: &AppHandle<R>) {
    log::info!("Tray: update_tray_menu_async called");
    // Get the current recording state
    let recording_state = get_current_recording_state().await;
    log::info!("Tray: Current recording state: {:?}", recording_state);

    // Determine if recording should be allowed
    // Only block recording during incomplete onboarding when no transcription model is ready
    let can_record = check_can_record(app).await;
    log::info!("Tray: can_record: {}", can_record);

    let recent_meetings = load_recent_meetings(app).await;
    if let Ok(mut cache) = RECENT_MEETINGS_CACHE.lock() {
        *cache = recent_meetings;
    }

    if let Ok(menu) = build_menu(app, recording_state, can_record) {
        if let Some(tray) = app.tray_by_id("main-tray") {
            let result = tray.set_menu(Some(menu));
            log::info!("Tray: Menu update result: {:?}", result);
            update_tray_status(&tray, recording_state);
        } else {
            log::warn!("Tray: Could not find tray with id 'main-tray'");
        }
    } else {
        log::error!("Tray: Failed to build menu");
    }
}

fn tray_tooltip(state: RecordingState) -> &'static str {
    match state {
        RecordingState::Stopped => "Minutes is active",
        RecordingState::Starting => "Minutes is starting a recording",
        RecordingState::Recording => "Minutes is recording",
        RecordingState::Pausing => "Minutes is pausing the recording",
        RecordingState::Paused => "Minutes recording is paused",
        RecordingState::Resuming => "Minutes is resuming the recording",
        RecordingState::Stopping => "Minutes is finishing the recording",
    }
}

fn tray_icon(state: RecordingState, detected: bool) -> (Image<'static>, bool) {
    const SIZE: u32 = 18;
    let mut pixels = vec![0; (SIZE * SIZE * 4) as usize];
    let mut set_pixel = |x: u32, y: u32, color: [u8; 4]| {
        let offset = ((y * SIZE + x) * 4) as usize;
        pixels[offset..offset + 4].copy_from_slice(&color);
    };

    match state {
        RecordingState::Recording => {
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let dx = x as f32 - 8.5;
                    let dy = y as f32 - 8.5;
                    if dx * dx + dy * dy <= 42.25 {
                        set_pixel(x, y, [235, 64, 52, 255]);
                    }
                }
            }
            (Image::new_owned(pixels, SIZE, SIZE), false)
        }
        RecordingState::Paused => {
            for y in 3..15 {
                for x in [5, 6, 11, 12] {
                    set_pixel(x, y, [0, 0, 0, 255]);
                }
            }
            (Image::new_owned(pixels, SIZE, SIZE), true)
        }
        _ => {
            const M: [&str; 7] = [
                "10001", "11011", "10101", "10101", "10001", "10001", "10001",
            ];

            // Detected: the same mark drawn in solid accent green. Non-template so
            // the colour actually shows in the menu bar.
            if detected {
                for (row, pattern) in M.iter().enumerate() {
                    for (column, value) in pattern.bytes().enumerate() {
                        if value == b'1' {
                            for dy in 0..2 {
                                for dx in 0..2 {
                                    set_pixel(
                                        4 + column as u32 * 2 + dx,
                                        2 + row as u32 * 2 + dy,
                                        [52, 199, 89, 255],
                                    );
                                }
                            }
                        }
                    }
                }
                return (Image::new_owned(pixels, SIZE, SIZE), false);
            }

            for (row, pattern) in M.iter().enumerate() {
                for (column, value) in pattern.bytes().enumerate() {
                    if value == b'1' {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                set_pixel(
                                    4 + column as u32 * 2 + dx,
                                    2 + row as u32 * 2 + dy,
                                    [0, 0, 0, 255],
                                );
                            }
                        }
                    }
                }
            }
            (Image::new_owned(pixels, SIZE, SIZE), true)
        }
    }
}

fn update_tray_status<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, state: RecordingState) {
    let detected = matches!(state, RecordingState::Stopped) && detected_meeting().is_some();
    let (icon, icon_as_template) = tray_icon(state, detected);
    if let Err(error) = tray.set_icon_with_as_template(Some(icon), icon_as_template) {
        log::warn!("Tray: Failed to set status icon: {}", error);
    }

    let tooltip = if detected {
        format!(
            "{} — meeting detected: {}",
            crate::APP_NAME,
            detected_meeting().unwrap_or_default()
        )
    } else {
        tray_tooltip(state).to_string()
    };
    if let Err(error) = tray.set_tooltip(Some(tooltip)) {
        log::warn!("Tray: Failed to set status tooltip: {}", error);
    }
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    state: RecordingState,
    can_record: bool, // True if recording is allowed (onboarding complete OR transcription model ready)
) -> tauri::Result<tauri::menu::Menu<R>> {
    let mut builder = MenuBuilder::new(app);
    let recent_meetings = cached_recent_meetings();

    // Meeting detected while idle: offer a one-click start at the top of the menu.
    if matches!(state, RecordingState::Stopped) {
        if let Some(name) = detected_meeting() {
            builder = builder
                .item(
                    &MenuItemBuilder::new(format!("🟢 {} detected", name))
                        .enabled(false)
                        .build(app)?,
                )
                .item(
                    &MenuItemBuilder::with_id("start_detected_recording", "🎙 Start recording")
                        .build(app)?,
                )
                .item(&MenuItemBuilder::with_id("dismiss_detected", "Dismiss").build(app)?)
                .item(&PredefinedMenuItem::separator(app)?);
        }
    }

    // If recording is not allowed (during onboarding, no transcription model), show disabled message
    if !can_record {
        builder = builder.item(
            &MenuItemBuilder::new("⏳ Downloading transcription model...")
                .enabled(false)
                .build(app)?,
        );
    } else {
        match state {
            RecordingState::Stopped => {
                builder = builder.item(
                    &MenuItemBuilder::with_id("toggle_recording", "Start Recording").build(app)?,
                );
            }
            RecordingState::Starting => {
                builder = builder.item(
                    &MenuItemBuilder::new("🔄 Starting Recording...")
                        .enabled(false)
                        .build(app)?,
                );
            }
            RecordingState::Recording => {
                builder = builder
                    .item(
                        &MenuItemBuilder::new(format!("🔴 Recording — {}", crate::APP_NAME))
                            .enabled(false)
                            .build(app)?,
                    )
                    .item(&PredefinedMenuItem::separator(app)?)
                    .item(
                        &MenuItemBuilder::with_id("pause_recording", "⏸ Pause Recording")
                            .build(app)?,
                    )
                    .item(
                        &MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording")
                            .build(app)?,
                    );
            }
            RecordingState::Pausing => {
                builder = builder
                    .item(
                        &MenuItemBuilder::new("⏸ Pausing...")
                            .enabled(false)
                            .build(app)?,
                    )
                    .item(
                        &MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording")
                            .build(app)?,
                    );
            }
            RecordingState::Paused => {
                builder = builder
                    .item(
                        &MenuItemBuilder::new("⏸ Recording paused")
                            .enabled(false)
                            .build(app)?,
                    )
                    .item(&PredefinedMenuItem::separator(app)?)
                    .item(
                        &MenuItemBuilder::with_id("resume_recording", "▶ Resume Recording")
                            .build(app)?,
                    )
                    .item(
                        &MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording")
                            .build(app)?,
                    );
            }
            RecordingState::Resuming => {
                builder = builder
                    .item(
                        &MenuItemBuilder::new("▶ Resuming...")
                            .enabled(false)
                            .build(app)?,
                    )
                    .item(
                        &MenuItemBuilder::with_id("stop_recording", "⏹ Stop Recording")
                            .build(app)?,
                    );
            }
            RecordingState::Stopping => {
                builder = builder.item(
                    &MenuItemBuilder::new("⏹ Stopping...")
                        .enabled(false)
                        .build(app)?,
                );
            }
        }
    }

    builder
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&build_recent_meetings_submenu(app, &recent_meetings)?)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&MenuItemBuilder::with_id("open_window", "Open Main Window").build(app)?)
        .item(&MenuItemBuilder::with_id("settings", "Settings").build(app)?)
        .item(&MenuItemBuilder::with_id("check_updates", "Check for Updates").build(app)?)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&MenuItemBuilder::with_id("quit", "Quit").build(app)?)
        .build()
}

fn build_recent_meetings_submenu<R: Runtime>(
    app: &AppHandle<R>,
    recent_meetings: &[(String, String)],
) -> tauri::Result<tauri::menu::Submenu<R>> {
    let mut submenu = SubmenuBuilder::new(app, "Recent Meetings");

    if recent_meetings.is_empty() {
        submenu = submenu.item(
            &MenuItemBuilder::new("No meetings yet")
                .enabled(false)
                .build(app)?,
        );
    } else {
        for (meeting_id, title) in recent_meetings {
            let item = MenuItemBuilder::with_id(format!("recent_meeting:{meeting_id}"), title)
                .build(app)?;
            submenu = submenu.item(&item);
        }
    }

    submenu.build()
}

/// Read the most recent meetings for the tray submenu.
///
/// Returns an empty list before the database is initialized or on query errors,
/// so the tray can always render.
async fn load_recent_meetings<R: Runtime>(app: &AppHandle<R>) -> Vec<(String, String)> {
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return Vec::new();
    };

    match crate::database::repositories::meeting::MeetingsRepository::get_meetings(
        state.db_manager.pool(),
    )
    .await
    {
        Ok(meetings) => meetings
            .into_iter()
            .take(RECENT_MEETINGS_LIMIT)
            .map(|meeting| {
                let label = recent_meeting_label(&meeting.title, meeting.created_at.0);
                (meeting.id, label)
            })
            .collect(),
        Err(error) => {
            log::warn!("Tray: failed to load recent meetings: {}", error);
            Vec::new()
        }
    }
}

/// Hide the main window when it is frontmost, otherwise bring it forward.
pub fn toggle_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    let visible = window.is_visible().unwrap_or(false);
    let minimized = window.is_minimized().unwrap_or(false);

    if visible && !minimized {
        if let Err(error) = window.hide() {
            log::error!("Failed to hide main window: {}", error);
        }
    } else {
        focus_main_window(app);
    }
}

pub(crate) fn focus_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.unminimize() {
            log::error!("Failed to unminimize main window: {}", e);
        }

        if let Err(e) = window.show() {
            log::error!("Failed to show main window: {}", e);
        }

        if let Err(e) = window.set_focus() {
            log::error!("Failed to focus main window: {}", e);
        }

        if let Err(e) = window.eval("window.focus()") {
            log::error!("Failed to focus main webview: {}", e);
        }
    } else {
        log::warn!("Could not find main window");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format_elapsed, recent_meeting_label, tray_icon, tray_tooltip, truncate_title,
        RecordingState,
    };
    use chrono::TimeZone;

    #[test]
    fn elapsed_time_uses_compact_under_an_hour() {
        assert_eq!(format_elapsed(0), "0:00");
        assert_eq!(format_elapsed(9), "0:09");
        assert_eq!(format_elapsed(65), "1:05");
        assert_eq!(format_elapsed(599), "9:59");
        assert_eq!(format_elapsed(3599), "59:59");
    }

    #[test]
    fn elapsed_time_adds_hours_past_sixty_minutes() {
        assert_eq!(format_elapsed(3600), "1:00:00");
        assert_eq!(format_elapsed(3661), "1:01:01");
        assert_eq!(format_elapsed(86399), "23:59:59");
    }

    #[test]
    fn meeting_titles_are_truncated_for_the_tray() {
        assert_eq!(truncate_title("  Standup  ", 40), "Standup");
        assert_eq!(truncate_title("", 40), "Untitled meeting");
        assert_eq!(truncate_title("Weekly planning", 6), "Weekly…");
    }

    #[test]
    fn recent_meeting_label_appends_the_title() {
        let created = chrono::Utc.with_ymd_and_hms(2026, 9, 11, 12, 0, 0).unwrap();
        let label = recent_meeting_label("Weekly planning", created);
        assert!(
            label.ends_with(" · Weekly planning"),
            "unexpected label: {label}"
        );
    }

    #[test]
    fn tray_status_distinguishes_idle_recording_and_paused_states() {
        assert_eq!(tray_tooltip(RecordingState::Stopped), "Minutes is active");
        assert_eq!(
            tray_tooltip(RecordingState::Recording),
            "Minutes is recording"
        );
        assert_eq!(
            tray_tooltip(RecordingState::Paused),
            "Minutes recording is paused"
        );

        let (idle_icon, idle_is_template) = tray_icon(RecordingState::Stopped, false);
        let (recording_icon, recording_is_template) = tray_icon(RecordingState::Recording, false);
        assert!(idle_is_template);
        assert!(!recording_is_template);
        assert!(idle_icon.rgba().chunks_exact(4).any(|pixel| pixel[3] > 0));
        assert!(recording_icon
            .rgba()
            .chunks_exact(4)
            .any(|pixel| pixel[0] == 235));
    }

    #[test]
    fn detected_icon_differs_from_idle() {
        let (idle, idle_is_template) = tray_icon(RecordingState::Stopped, false);
        let (detected, detected_is_template) = tray_icon(RecordingState::Stopped, true);
        assert!(idle_is_template);
        assert!(!detected_is_template);
        assert_ne!(idle.rgba(), detected.rgba());
        let has_accent = detected
            .rgba()
            .chunks_exact(4)
            .any(|pixel| pixel[0] == 52 && pixel[1] == 199 && pixel[2] == 89);
        assert!(
            has_accent,
            "detected icon should use the high-contrast accent"
        );
    }
}
