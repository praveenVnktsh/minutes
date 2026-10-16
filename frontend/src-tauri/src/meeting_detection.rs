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
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "meeting_detection.json";
const STORE_KEY: &str = "settings";
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
/// normalized names match. A name entry also matches a known meeting app by its
/// display name, so typing "Zoom" ignores the `zoom.us` process.
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
    known_meeting_app(&app_name).is_some_and(|display| normalize_app_name(display) == entry_name)
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

fn known_meeting_app(lower: &str) -> Option<&'static str> {
    const CONTAINS: &[(&str, &str)] = &[
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
    for (token, display) in CONTAINS {
        if lower.contains(token) {
            return Some(display);
        }
    }

    const EXACT: &[(&str, &str)] = &[
        ("teams", "Microsoft Teams"),
        ("slack", "Slack Huddle"),
        ("discord", "Discord"),
        ("facetime", "FaceTime"),
        ("telegram", "Telegram"),
        ("signal", "Signal"),
    ];
    for (token, display) in EXACT {
        if lower == *token {
            return Some(display);
        }
    }

    None
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
static SNOOZED_UNTIL: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

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

/// Load the settings from the store into the in-memory cache.
pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<MeetingDetectionSettings> {
    let store = app
        .store(STORE_FILE)
        .map_err(|error| anyhow::anyhow!("Failed to open {STORE_FILE}: {error}"))?;
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
    let store = app
        .store(STORE_FILE)
        .map_err(|error| anyhow::anyhow!("Failed to open {STORE_FILE}: {error}"))?;
    store.set(STORE_KEY, serde_json::to_value(settings)?);
    store
        .save()
        .map_err(|error| anyhow::anyhow!("Failed to save {STORE_FILE}: {error}"))?;
    set_cached_settings(settings.clone());
    Ok(())
}

/// Seconds since the Unix epoch when the snooze ends, if detection is snoozed.
fn snoozed_until_epoch() -> Option<u64> {
    let mut guard = SNOOZED_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
    match *guard {
        Some((until, epoch)) if Instant::now() < until => Some(epoch),
        Some(_) => {
            *guard = None;
            None
        }
        None => None,
    }
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

/// Re-apply the settings to whatever is capturing the mic now and update the
/// tray. With `hide_prompt`, also hide the prompt when nothing is left to offer;
/// the prompt's own "Don't ask" flow passes false so its Undo stays visible.
fn reevaluate<R: Runtime>(app: &AppHandle<R>, hide_prompt: bool) {
    let detected = current_detection();
    let nothing_to_offer = detected.is_none();
    crate::tray::set_meeting_detected(app, detected.map(|d| d.display_name));
    if hide_prompt && nothing_to_offer {
        if let Some(window) = app.get_webview_window("meeting-prompt") {
            let _ = window.hide();
        }
    }
}

// --- commands --------------------------------------------------------------

#[tauri::command]
pub fn get_meeting_detection_settings<R: Runtime>(
    app: AppHandle<R>,
) -> Result<MeetingDetectionSettings, String> {
    load_settings(&app).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_meeting_detection_settings<R: Runtime>(
    app: AppHandle<R>,
    settings: MeetingDetectionSettings,
) -> Result<MeetingDetectionSettings, String> {
    save_settings(&app, &settings).map_err(|error| error.to_string())?;
    reevaluate(&app, true);
    Ok(settings)
}

/// Add an app to the ignore list (from the prompt or from settings).
#[tauri::command]
pub fn ignore_meeting_app<R: Runtime>(
    app: AppHandle<R>,
    target: AudioApp,
) -> Result<MeetingDetectionSettings, String> {
    let mut settings = load_settings(&app).map_err(|error| error.to_string())?;
    if add_ignored(&mut settings, &target) {
        save_settings(&app, &settings).map_err(|error| error.to_string())?;
        log::info!(
            "Meeting detection: ignoring {} ({:?})",
            target.name,
            target.bundle_id
        );
    }
    reevaluate(&app, false);
    Ok(settings)
}

/// Remove an app from the ignore list (settings row, or Undo on the prompt).
#[tauri::command]
pub fn unignore_meeting_app<R: Runtime>(
    app: AppHandle<R>,
    target: AudioApp,
) -> Result<MeetingDetectionSettings, String> {
    let mut settings = load_settings(&app).map_err(|error| error.to_string())?;
    if remove_ignored(&mut settings, &target) {
        save_settings(&app, &settings).map_err(|error| error.to_string())?;
    }
    reevaluate(&app, false);
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
    let duration = Duration::from_secs(u64::from(minutes.clamp(1, 24 * 60)) * 60);
    let until_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| (now + duration).as_secs())
        .unwrap_or_default();
    if let Ok(mut guard) = SNOOZED_UNTIL.lock() {
        *guard = Some((Instant::now() + duration, until_epoch));
    }
    log::info!("Meeting detection: snoozed for {minutes} minutes");
    crate::meeting_prompt::dismiss_meeting_prompt(app.clone());
    reevaluate(&app, true);
    until_epoch
}

/// When the current snooze ends (Unix seconds), if one is active.
#[tauri::command]
pub fn get_meeting_detection_snooze() -> Option<u64> {
    snoozed_until_epoch()
}

#[tauri::command]
pub fn resume_meeting_detection<R: Runtime>(app: AppHandle<R>) {
    if let Ok(mut guard) = SNOOZED_UNTIL.lock() {
        *guard = None;
    }
    reevaluate(&app, true);
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
