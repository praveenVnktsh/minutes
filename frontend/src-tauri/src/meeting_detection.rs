//! Identify a meeting app from the processes currently capturing the microphone,
//! and decide whether Minutes should offer to record it.
//!
//! Runs in Rust so the tray can update the moment the mic starts, without
//! waiting on the webview (which macOS can throttle when the window is hidden).
//!
//! The user can tell Minutes never to ask for an app (the ignore list), only to
//! ask for known meeting apps, or to snooze detection for a while. All of those
//! are applied here, so every surface (tray, floating prompt) agrees.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{path::BaseDirectory, AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "meeting_detection.json";
const STORE_KEY: &str = "settings";
const SNOOZE_KEY: &str = "snoozed_until";
const RECENT_APPS_LIMIT: usize = 12;

/// An app capturing the microphone. `bundle_id` is the macOS bundle identifier
/// when the process list can resolve one; it is the most stable way to match an
/// app, since the display name is localized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioApp {
    pub name: String,
    #[serde(default)]
    pub bundle_id: Option<String>,
}

impl AudioApp {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            bundle_id: None,
        }
    }
}

/// Persisted meeting detection choices.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeetingDetectionSettings {
    /// Apps Minutes never asks about, even when they are known meeting apps.
    #[serde(default)]
    pub ignored_apps: Vec<AudioApp>,
    /// When on, unknown apps never trigger the prompt; only known meeting apps do.
    #[serde(default)]
    pub only_known_apps: bool,
}

/// A detected meeting: the name to show and the app that triggered it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DetectedMeeting {
    pub display_name: String,
    pub app: AudioApp,
}

/// Lowercase, trim, drop a trailing `.app` and collapse whitespace, so
/// "Superwhisper.app", " superwhisper " and "SuperWhisper" compare equal.
pub fn normalize_app_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let stripped = lower.strip_suffix(".app").unwrap_or(&lower);
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `entry` (from the ignore list) refers to `app`.
///
/// Bundle ids match when both sides have one; otherwise (or additionally) the
/// normalized names match. A name entry that names a known meeting app (its
/// display name or an alias like "Teams") covers every process that known-app
/// rule matches, so "Teams" also ignores "Microsoft Teams (work or school)".
fn entry_matches(entry: &AudioApp, app: &AudioApp) -> bool {
    if let (Some(a), Some(b)) = (&entry.bundle_id, &app.bundle_id) {
        if a.eq_ignore_ascii_case(b) {
            return true;
        }
    }
    let entry_name = normalize_app_name(&entry.name);
    if entry_name.is_empty() {
        return false;
    }
    let app_name = normalize_app_name(&app.name);
    if entry_name == app_name {
        return true;
    }
    match known_app_named(&entry_name) {
        Some(display) => known_meeting_app(&app_name) == Some(display),
        None => false,
    }
}

pub fn is_ignored(app: &AudioApp, ignored: &[AudioApp]) -> bool {
    ignored.iter().any(|entry| entry_matches(entry, app))
}

/// Return the meeting app to offer recording for, or `None`.
///
/// Ignored apps are skipped first, so a user's choice wins over the known list.
/// Known apps win over unknown ones. Unknown apps fall back to the first
/// non-media/browser app so generic call apps still surface, unless
/// `only_known_apps` is on.
pub fn detect_meeting_app(
    apps: &[AudioApp],
    settings: &MeetingDetectionSettings,
) -> Option<DetectedMeeting> {
    let mut generic: Option<DetectedMeeting> = None;

    for app in apps {
        let name = app.name.trim();
        if name.is_empty() || is_ignored(app, &settings.ignored_apps) {
            continue;
        }
        let lower = normalize_app_name(name);

        if let Some(display) = known_meeting_app(&lower) {
            return Some(DetectedMeeting {
                display_name: display.to_string(),
                app: app.clone(),
            });
        }
        if generic.is_none() && !settings.only_known_apps && !is_non_meeting_audio(&lower) {
            generic = Some(DetectedMeeting {
                display_name: name.to_string(),
                app: app.clone(),
            });
        }
    }

    generic
}

/// Known meeting apps: a process whose normalized name contains the token.
const KNOWN_CONTAINS: &[(&str, &str)] = &[
    ("google meet", "Google Meet"),
    ("zoom", "Zoom"),
    ("microsoft teams", "Microsoft Teams"),
    ("webex", "Cisco Webex"),
    ("amazon chime", "Amazon Chime"),
    ("gotomeeting", "GoTo Meeting"),
    ("go to meeting", "GoTo Meeting"),
    ("bluejeans", "BlueJeans"),
    ("whatsapp", "WhatsApp"),
];

/// Known meeting apps: a process whose normalized name is exactly the token.
const KNOWN_EXACT: &[(&str, &str)] = &[
    ("teams", "Microsoft Teams"),
    ("slack", "Slack Huddle"),
    ("discord", "Discord"),
    ("facetime", "FaceTime"),
    ("telegram", "Telegram"),
    ("signal", "Signal"),
];

fn known_meeting_app(lower: &str) -> Option<&'static str> {
    KNOWN_CONTAINS
        .iter()
        .find(|(token, _)| lower.contains(token))
        .or_else(|| KNOWN_EXACT.iter().find(|(token, _)| lower == *token))
        .map(|(_, display)| *display)
}

/// The known meeting app a user-entered name refers to, when it is exactly
/// that app's display name or one of its aliases ("Teams", "Zoom", "Slack").
fn known_app_named(entry: &str) -> Option<&'static str> {
    KNOWN_CONTAINS
        .iter()
        .chain(KNOWN_EXACT)
        .find(|(token, display)| *token == entry || normalize_app_name(display) == entry)
        .map(|(_, display)| *display)
}

/// Apps that use audio but are not meetings: media, browsers (probed for Meet
/// separately), Minutes itself, and dictation/voice-assistant tools that open
/// the mic for a few seconds at a time.
fn is_non_meeting_audio(lower: &str) -> bool {
    const EXACT: &[&str] = &["arc", "siri", "claude", "chatgpt"];
    if EXACT.contains(&lower) {
        return true;
    }
    const TOKENS: &[&str] = &[
        "spotify",
        "music",
        "podcast",
        "vlc",
        "quicktime",
        "imovie",
        "garageband",
        "audacity",
        "safari",
        "google chrome",
        "chrome",
        "microsoft edge",
        "edge",
        "firefox",
        "brave",
        "chromium",
        "opera",
        "minutes",
        "meetily",
        "coreaudio",
        // Dictation and voice tools.
        "superwhisper",
        "wispr flow",
        "macwhisper",
        "voice control",
        "dictation",
        "voice memos",
    ];
    TOKENS.iter().any(|token| lower.contains(token))
}

/// Add `app` to the ignore list unless an entry already covers it. Returns
/// whether the list changed.
pub fn add_ignored(settings: &mut MeetingDetectionSettings, app: &AudioApp) -> bool {
    let name = app.name.trim();
    if name.is_empty() && app.bundle_id.is_none() {
        return false;
    }
    if is_ignored(app, &settings.ignored_apps) {
        return false;
    }
    settings.ignored_apps.push(AudioApp {
        name: name.to_string(),
        bundle_id: app.bundle_id.clone(),
    });
    true
}

/// Remove the entry for `app`. Matches exactly (name + bundle id) first, then
/// by normalized name, so a settings row and an undo both remove what they added.
pub fn remove_ignored(settings: &mut MeetingDetectionSettings, app: &AudioApp) -> bool {
    let before = settings.ignored_apps.len();
    if let Some(index) = settings.ignored_apps.iter().position(|entry| entry == app) {
        settings.ignored_apps.remove(index);
    } else {
        let target = normalize_app_name(&app.name);
        settings
            .ignored_apps
            .retain(|entry| normalize_app_name(&entry.name) != target);
    }
    settings.ignored_apps.len() != before
}

/// Push newly seen apps to the front of `recent`, newest first, capped.
fn remember_recent(recent: &mut Vec<AudioApp>, apps: &[AudioApp]) {
    for app in apps {
        let lower = normalize_app_name(&app.name);
        if lower.is_empty() || lower == "minutes" || lower == "meetily" {
            continue;
        }
        recent.retain(|seen| normalize_app_name(&seen.name) != lower);
        recent.insert(0, app.clone());
    }
    recent.truncate(RECENT_APPS_LIMIT);
}

// --- runtime state ---------------------------------------------------------

static SETTINGS: LazyLock<RwLock<MeetingDetectionSettings>> =
    LazyLock::new(|| RwLock::new(MeetingDetectionSettings::default()));
/// Apps capturing the mic right now, so a settings change can re-evaluate.
static ACTIVE_APPS: Mutex<Vec<AudioApp>> = Mutex::new(Vec::new());
static RECENT_APPS: Mutex<Vec<AudioApp>> = Mutex::new(Vec::new());
/// Wall-clock end of the snooze, in Unix seconds. Wall clock rather than
/// `Instant` because `Instant` stops while the Mac sleeps, which would stretch a
/// one-hour snooze overnight.
static SNOOZED_UNTIL: Mutex<Option<u64>> = Mutex::new(None);
/// Set once the store file has been checked (and backed up if unreadable).
static STORE_CHECKED: AtomicBool = AtomicBool::new(false);

/// Event the settings UI listens to so it never works from a stale copy.
pub const CHANGED_EVENT: &str = "meeting-detection-changed";

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.as_secs())
        .unwrap_or_default()
}

/// Whether a snooze ending at `until` (Unix seconds) is still running at `now`.
fn snooze_active(until: Option<u64>, now: u64) -> bool {
    until.is_some_and(|until| now < until)
}

fn cached_settings() -> MeetingDetectionSettings {
    SETTINGS
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default()
}

fn set_cached_settings(settings: MeetingDetectionSettings) {
    if let Ok(mut guard) = SETTINGS.write() {
        *guard = settings;
    }
}

/// If the store file exists but cannot be read as settings, copy it to
/// `<file>.bak` so the user's list survives the defaults being saved over it.
/// Returns the backup path when one was written.
fn back_up_unreadable_store(path: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(path).ok()?;
    let readable = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .is_some_and(|value| match value.get(STORE_KEY) {
            Some(settings) => {
                serde_json::from_value::<MeetingDetectionSettings>(settings.clone()).is_ok()
            }
            None => value.is_object(),
        });
    if readable {
        return None;
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = PathBuf::from(backup);
    match std::fs::copy(path, &backup) {
        Ok(_) => {
            log::warn!(
                "Meeting detection: {} was unreadable; saved a copy to {} and using defaults",
                path.display(),
                backup.display()
            );
            Some(backup)
        }
        Err(error) => {
            log::warn!(
                "Meeting detection: {} is unreadable and could not be backed up: {error}",
                path.display()
            );
            None
        }
    }
}

fn open_store<R: Runtime>(
    app: &AppHandle<R>,
) -> anyhow::Result<std::sync::Arc<tauri_plugin_store::Store<R>>> {
    if !STORE_CHECKED.swap(true, Ordering::SeqCst) {
        // The store plugin resolves relative paths against the app data dir and
        // silently starts empty when the file cannot be parsed.
        if let Ok(path) = app.path().resolve(STORE_FILE, BaseDirectory::AppData) {
            back_up_unreadable_store(&path);
        }
    }
    app.store(STORE_FILE)
        .map_err(|error| anyhow::anyhow!("Failed to open {STORE_FILE}: {error}"))
}

/// Load the settings (and any persisted snooze) from the store into memory.
pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<MeetingDetectionSettings> {
    let store = open_store(app)?;
    let settings = match store.get(STORE_KEY) {
        Some(value) => serde_json::from_value(value).unwrap_or_else(|error| {
            log::warn!("Meeting detection: unreadable settings, using defaults: {error}");
            MeetingDetectionSettings::default()
        }),
        None => MeetingDetectionSettings::default(),
    };
    set_cached_settings(settings.clone());
    Ok(settings)
}

fn save_settings<R: Runtime>(
    app: &AppHandle<R>,
    settings: &MeetingDetectionSettings,
) -> anyhow::Result<()> {
    let store = open_store(app)?;
    store.set(STORE_KEY, serde_json::to_value(settings)?);
    store
        .save()
        .map_err(|error| anyhow::anyhow!("Failed to save {STORE_FILE}: {error}"))?;
    set_cached_settings(settings.clone());
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

/// Read-modify-write the stored settings so concurrent edits (prompt and
/// settings window) each apply to the latest list rather than a stale copy.
fn update_settings<R: Runtime>(
    app: &AppHandle<R>,
    change: impl FnOnce(&mut MeetingDetectionSettings) -> bool,
) -> anyhow::Result<MeetingDetectionSettings> {
    static WRITE_LOCK: Mutex<()> = Mutex::new(());
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut settings = load_settings(app)?;
    if change(&mut settings) {
        save_settings(app, &settings)?;
    }
    Ok(settings)
}

fn persist_snooze<R: Runtime>(app: &AppHandle<R>, until: Option<u64>) {
    if let Ok(mut guard) = SNOOZED_UNTIL.lock() {
        *guard = until;
    }
    match open_store(app) {
        Ok(store) => {
            match until {
                Some(until) => store.set(SNOOZE_KEY, serde_json::json!(until)),
                None => {
                    store.delete(SNOOZE_KEY);
                }
            }
            if let Err(error) = store.save() {
                log::warn!("Meeting detection: could not save snooze: {error}");
            }
        }
        Err(error) => log::warn!("Meeting detection: could not save snooze: {error}"),
    }
    let _ = app.emit(CHANGED_EVENT, ());
}

/// Restore a snooze saved before a restart, and watch for it to end.
pub fn restore_snooze<R: Runtime>(app: &AppHandle<R>) {
    let Ok(store) = open_store(app) else { return };
    let until = store.get(SNOOZE_KEY).and_then(|value| value.as_u64());
    if snooze_active(until, now_epoch()) {
        if let Ok(mut guard) = SNOOZED_UNTIL.lock() {
            *guard = until;
        }
        spawn_snooze_watcher(app.clone(), until.unwrap_or_default());
    } else if until.is_some() {
        persist_snooze(app, None);
    }
}

/// Ends the snooze on time by wall clock and re-evaluates what is using the
/// mic, so a meeting already in progress is offered as soon as it ends.
fn spawn_snooze_watcher<R: Runtime>(app: AppHandle<R>, until: u64) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let current = SNOOZED_UNTIL.lock().map(|guard| *guard).unwrap_or(None);
            if current != Some(until) {
                return; // resumed or snoozed again; that call owns the watch
            }
            if !snooze_active(current, now_epoch()) {
                log::info!("Meeting detection: snooze ended");
                persist_snooze(&app, None);
                reevaluate(&app, PromptUpdate::ShowOrHide);
                return;
            }
        }
    });
}

/// Unix seconds when the snooze ends, if detection is snoozed now.
fn snoozed_until_epoch() -> Option<u64> {
    let until = SNOOZED_UNTIL.lock().map(|guard| *guard).unwrap_or(None);
    until.filter(|_| snooze_active(until, now_epoch()))
}

/// Record the apps now capturing the mic and return the meeting to offer, with
/// the ignore list, the known-only toggle and any snooze applied.
pub fn on_active_apps(apps: &[AudioApp]) -> Option<DetectedMeeting> {
    if let Ok(mut active) = ACTIVE_APPS.lock() {
        *active = apps.to_vec();
    }
    if let Ok(mut recent) = RECENT_APPS.lock() {
        remember_recent(&mut recent, apps);
    }
    current_detection()
}

/// The mic stopped: nothing is active any more.
pub fn on_audio_stopped() {
    if let Ok(mut active) = ACTIVE_APPS.lock() {
        active.clear();
    }
}

fn current_detection() -> Option<DetectedMeeting> {
    if snoozed_until_epoch().is_some() {
        return None;
    }
    let apps = ACTIVE_APPS.lock().map(|a| a.clone()).unwrap_or_default();
    detect_meeting_app(&apps, &cached_settings())
}

/// What a re-evaluation may do to the floating prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptUpdate {
    /// Show the prompt for what is detected now, or hide it if nothing is.
    ShowOrHide,
    /// Leave the prompt alone: the prompt itself started this change and is
    /// showing its own confirmation with Undo.
    TrayOnly,
}

/// Re-apply the settings and snooze to whatever is capturing the mic now, so
/// the tray and the prompt reflect them straight away.
fn reevaluate<R: Runtime>(app: &AppHandle<R>, prompt: PromptUpdate) {
    let detected = current_detection();
    crate::tray::set_meeting_detected(app, detected.as_ref().map(|d| d.display_name.clone()));
    if prompt == PromptUpdate::TrayOnly {
        return;
    }
    match detected {
        Some(detected) => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::meeting_prompt::maybe_show(&app, &detected).await;
            });
        }
        None => {
            if let Some(window) = app.get_webview_window("meeting-prompt") {
                let _ = window.hide();
            }
        }
    }
}

fn prompt_update_for(source: Option<&str>) -> PromptUpdate {
    if source == Some("prompt") {
        PromptUpdate::TrayOnly
    } else {
        PromptUpdate::ShowOrHide
    }
}

// --- commands --------------------------------------------------------------

#[tauri::command]
pub fn get_meeting_detection_settings<R: Runtime>(
    app: AppHandle<R>,
) -> Result<MeetingDetectionSettings, String> {
    load_settings(&app).map_err(|error| error.to_string())
}

/// Turn the "only known meeting apps" option on or off without touching the
/// ignore list.
#[tauri::command]
pub fn set_only_known_meeting_apps<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
) -> Result<MeetingDetectionSettings, String> {
    let settings = update_settings(&app, |settings| {
        let changed = settings.only_known_apps != enabled;
        settings.only_known_apps = enabled;
        changed
    })
    .map_err(|error| error.to_string())?;
    reevaluate(&app, PromptUpdate::ShowOrHide);
    Ok(settings)
}

/// Add an app to the ignore list. `source` is "prompt" when the floating
/// prompt asked, so it can keep showing its Undo.
#[tauri::command]
pub fn ignore_meeting_app<R: Runtime>(
    app: AppHandle<R>,
    target: AudioApp,
    source: Option<String>,
) -> Result<MeetingDetectionSettings, String> {
    let settings = update_settings(&app, |settings| add_ignored(settings, &target))
        .map_err(|error| error.to_string())?;
    log::info!(
        "Meeting detection: ignoring {} ({:?})",
        target.name,
        target.bundle_id
    );
    reevaluate(&app, prompt_update_for(source.as_deref()));
    Ok(settings)
}

/// Remove an app from the ignore list (settings row, or Undo on the prompt).
#[tauri::command]
pub fn unignore_meeting_app<R: Runtime>(
    app: AppHandle<R>,
    target: AudioApp,
    source: Option<String>,
) -> Result<MeetingDetectionSettings, String> {
    let settings = update_settings(&app, |settings| remove_ignored(settings, &target))
        .map_err(|error| error.to_string())?;
    reevaluate(&app, prompt_update_for(source.as_deref()));
    Ok(settings)
}

/// Apps seen capturing the mic recently, newest first, for settings suggestions.
#[tauri::command]
pub fn get_recent_audio_apps() -> Vec<AudioApp> {
    RECENT_APPS.lock().map(|r| r.clone()).unwrap_or_default()
}

/// Stop detecting meetings for `minutes`. Returns when the snooze ends (Unix seconds).
#[tauri::command]
pub fn snooze_meeting_detection<R: Runtime>(app: AppHandle<R>, minutes: u32) -> u64 {
    let until = now_epoch() + u64::from(minutes.clamp(1, 24 * 60)) * 60;
    persist_snooze(&app, Some(until));
    log::info!("Meeting detection: snoozed for {minutes} minutes");
    // Cancel any unclaimed start request, but do not mark the prompt dismissed:
    // the snooze suppresses it, and when the snooze ends a meeting already in
    // progress should be offered.
    crate::meeting_prompt::cancel_pending_request(&app);
    reevaluate(&app, PromptUpdate::ShowOrHide);
    spawn_snooze_watcher(app, until);
    until
}

/// When the current snooze ends (Unix seconds), if one is active.
#[tauri::command]
pub fn get_meeting_detection_snooze() -> Option<u64> {
    snoozed_until_epoch()
}

#[tauri::command]
pub fn resume_meeting_detection<R: Runtime>(app: AppHandle<R>) {
    persist_snooze(&app, None);
    reevaluate(&app, PromptUpdate::ShowOrHide);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(names: &[&str]) -> Vec<AudioApp> {
        names.iter().map(|name| AudioApp::named(*name)).collect()
    }

    fn detect(names: &[&str]) -> Option<String> {
        detect_meeting_app(&apps(names), &MeetingDetectionSettings::default())
            .map(|d| d.display_name)
    }

    fn ignoring(entries: &[AudioApp]) -> MeetingDetectionSettings {
        MeetingDetectionSettings {
            ignored_apps: entries.to_vec(),
            only_known_apps: false,
        }
    }

    #[test]
    fn recognizes_known_meeting_apps() {
        assert_eq!(detect(&["zoom.us"]).as_deref(), Some("Zoom"));
        assert_eq!(
            detect(&["Microsoft Teams (work or school)"]).as_deref(),
            Some("Microsoft Teams")
        );
        assert_eq!(detect(&["WhatsApp"]).as_deref(), Some("WhatsApp"));
    }

    #[test]
    fn ignores_media_and_browsers() {
        assert_eq!(detect(&["Spotify"]), None);
        assert_eq!(detect(&["Google Chrome"]), None);
        assert_eq!(detect(&["Minutes"]), None);
    }

    #[test]
    fn dictation_and_voice_tools_are_not_meetings() {
        for name in [
            "Superwhisper",
            "Wispr Flow",
            "MacWhisper",
            "Voice Control",
            "Siri",
            "Claude",
            "ChatGPT",
            "Voice Memos",
        ] {
            assert_eq!(detect(&[name]), None, "{name} should not prompt");
        }
        // Exact-only entries do not swallow unrelated names that contain them.
        assert_eq!(
            detect(&["Claudette Calls"]).as_deref(),
            Some("Claudette Calls")
        );
    }

    #[test]
    fn falls_back_to_unknown_apps() {
        assert_eq!(detect(&["SomeCallApp"]).as_deref(), Some("SomeCallApp"));
    }

    #[test]
    fn known_apps_win_over_the_fallback() {
        assert_eq!(detect(&["SomeCallApp", "zoom.us"]).as_deref(), Some("Zoom"));
    }

    #[test]
    fn an_ignored_known_app_never_triggers() {
        let settings = ignoring(&[AudioApp::named("zoom.us")]);
        assert_eq!(detect_meeting_app(&apps(&["zoom.us"]), &settings), None);
        // Ignoring by the display name the prompt shows works too.
        let settings = ignoring(&[AudioApp::named("Zoom")]);
        assert_eq!(detect_meeting_app(&apps(&["zoom.us"]), &settings), None);
    }

    #[test]
    fn an_ignored_generic_app_never_triggers() {
        let settings = ignoring(&[AudioApp::named("SomeDictationTool")]);
        assert_eq!(
            detect_meeting_app(&apps(&["SomeDictationTool"]), &settings),
            None
        );
    }

    #[test]
    fn a_non_ignored_app_still_triggers_alongside_an_ignored_one() {
        let settings = ignoring(&[AudioApp::named("zoom.us")]);
        let detected = detect_meeting_app(&apps(&["zoom.us", "SomeCallApp"]), &settings).unwrap();
        assert_eq!(detected.display_name, "SomeCallApp");

        let settings = ignoring(&[AudioApp::named("SomeDictationTool")]);
        let detected =
            detect_meeting_app(&apps(&["SomeDictationTool", "Slack"]), &settings).unwrap();
        assert_eq!(detected.display_name, "Slack Huddle");
        assert_eq!(detected.app, AudioApp::named("Slack"));
    }

    #[test]
    fn only_known_apps_disables_the_generic_fallback() {
        let settings = MeetingDetectionSettings {
            ignored_apps: vec![],
            only_known_apps: true,
        };
        assert_eq!(detect_meeting_app(&apps(&["SomeCallApp"]), &settings), None);
        assert_eq!(
            detect_meeting_app(&apps(&["SomeCallApp", "zoom.us"]), &settings)
                .map(|d| d.display_name)
                .as_deref(),
            Some("Zoom")
        );
    }

    #[test]
    fn names_match_case_and_suffix_insensitively() {
        assert_eq!(normalize_app_name("  SuperWhisper.app "), "superwhisper");
        assert_eq!(normalize_app_name("Some   Call\tApp"), "some call app");
        let settings = ignoring(&[AudioApp::named("some call app")]);
        assert_eq!(
            detect_meeting_app(&apps(&["Some Call App.app"]), &settings),
            None
        );
    }

    #[test]
    fn a_typed_known_app_name_or_alias_covers_all_its_processes() {
        for entry in ["Teams", "Microsoft Teams", "microsoft teams"] {
            let settings = ignoring(&[AudioApp::named(entry)]);
            assert_eq!(
                detect_meeting_app(&apps(&["Microsoft Teams (work or school)"]), &settings),
                None,
                "{entry}"
            );
            assert_eq!(
                detect_meeting_app(&apps(&["Teams"]), &settings),
                None,
                "{entry}"
            );
        }
        let settings = ignoring(&[AudioApp::named("Slack")]);
        assert_eq!(detect_meeting_app(&apps(&["Slack"]), &settings), None);
        // Only an exact alias counts: a longer name that merely contains one does not.
        let settings = ignoring(&[AudioApp::named("Zoom Notes Helper")]);
        assert_eq!(
            detect_meeting_app(&apps(&["zoom.us"]), &settings)
                .map(|d| d.display_name)
                .as_deref(),
            Some("Zoom")
        );
    }

    #[test]
    fn snooze_expiry_follows_the_wall_clock() {
        assert!(snooze_active(Some(1_000), 999));
        assert!(!snooze_active(Some(1_000), 1_000));
        assert!(!snooze_active(None, 0));
        // An hour's snooze set before an overnight sleep is over in the morning.
        let set_at = 1_700_000_000;
        assert!(!snooze_active(Some(set_at + 3600), set_at + 9 * 3600));
    }

    #[test]
    fn an_unreadable_store_is_backed_up_before_defaults_take_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meeting_detection.json");

        assert_eq!(
            back_up_unreadable_store(&path),
            None,
            "missing file is fine"
        );

        std::fs::write(&path, br#"{"settings":{"ignored_apps":[{"name":"X"}]}}"#).unwrap();
        assert_eq!(
            back_up_unreadable_store(&path),
            None,
            "valid file is left alone"
        );

        std::fs::write(&path, b"{ not json").unwrap();
        let backup = back_up_unreadable_store(&path).expect("backed up");
        assert_eq!(backup, dir.path().join("meeting_detection.json.bak"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"{ not json");

        std::fs::write(&path, br#"{"settings":{"ignored_apps":"oops"}}"#).unwrap();
        assert!(
            back_up_unreadable_store(&path).is_some(),
            "wrong shape is backed up"
        );
    }

    #[test]
    fn only_the_prompt_keeps_its_own_view_on_ignore() {
        assert_eq!(prompt_update_for(Some("prompt")), PromptUpdate::TrayOnly);
        assert_eq!(
            prompt_update_for(Some("settings")),
            PromptUpdate::ShowOrHide
        );
        assert_eq!(prompt_update_for(None), PromptUpdate::ShowOrHide);
    }

    #[test]
    fn bundle_ids_match_even_when_the_name_changes() {
        let entry = AudioApp {
            name: "Tool".into(),
            bundle_id: Some("com.example.tool".into()),
        };
        let renamed = AudioApp {
            name: "Outil".into(),
            bundle_id: Some("COM.example.tool".into()),
        };
        assert_eq!(detect_meeting_app(&[renamed], &ignoring(&[entry])), None);
    }

    #[test]
    fn adding_and_removing_round_trips_for_undo() {
        let mut settings = MeetingDetectionSettings::default();
        let app = AudioApp {
            name: "Tool".into(),
            bundle_id: Some("com.example.tool".into()),
        };
        assert!(add_ignored(&mut settings, &app));
        assert!(!add_ignored(&mut settings, &app), "no duplicates");
        assert!(!add_ignored(&mut settings, &AudioApp::named("   ")));
        assert_eq!(settings.ignored_apps, vec![app.clone()]);
        assert!(remove_ignored(&mut settings, &app));
        assert!(settings.ignored_apps.is_empty());

        settings.ignored_apps.push(AudioApp::named("Some Tool"));
        assert!(remove_ignored(&mut settings, &AudioApp::named("some tool")));
        assert!(!remove_ignored(
            &mut settings,
            &AudioApp::named("some tool")
        ));
    }

    #[test]
    fn settings_persist_as_json_and_tolerate_missing_fields() {
        let settings = MeetingDetectionSettings {
            ignored_apps: vec![AudioApp {
                name: "Tool".into(),
                bundle_id: Some("com.example.tool".into()),
            }],
            only_known_apps: true,
        };
        let value = serde_json::to_value(&settings).unwrap();
        let restored: MeetingDetectionSettings = serde_json::from_value(value).unwrap();
        assert_eq!(restored, settings);

        let partial: MeetingDetectionSettings =
            serde_json::from_value(serde_json::json!({ "ignored_apps": [{ "name": "X" }] }))
                .unwrap();
        assert_eq!(partial.ignored_apps, vec![AudioApp::named("X")]);
        assert!(!partial.only_known_apps);
    }

    #[test]
    fn recent_apps_are_newest_first_deduplicated_and_capped() {
        let mut recent = Vec::new();
        remember_recent(&mut recent, &apps(&["A", "Minutes", "B"]));
        remember_recent(&mut recent, &apps(&["a"]));
        assert_eq!(
            recent.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "B"]
        );
        let many: Vec<String> = (0..20).map(|i| format!("App {i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        remember_recent(&mut recent, &apps(&many));
        assert_eq!(recent.len(), RECENT_APPS_LIMIT);
        assert_eq!(recent[0].name, "App 19");
    }
}
