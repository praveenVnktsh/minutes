use std::str::FromStr;
use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_store::StoreExt;

pub const DEFAULT_RECORDING: &str = "CmdOrCtrl+Shift+KeyR";
pub const DEFAULT_WINDOW: &str = "CmdOrCtrl+Shift+KeyM";
pub const DEFAULT_PAUSE: &str = "CmdOrCtrl+Shift+KeyP";

pub struct GlobalShortcuts {
    pub recording: String,
    pub window: String,
    pub pause: String,
}

fn store_string<R: Runtime>(app: &AppHandle<R>, key: &str, default: &str) -> String {
    app.store("store.json")
        .ok()
        .and_then(|store| store.get(key))
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| default.to_string())
}

/// The configured shortcuts (falling back to defaults).
pub fn current<R: Runtime>(app: &AppHandle<R>) -> GlobalShortcuts {
    GlobalShortcuts {
        recording: store_string(app, "shortcutRecording", DEFAULT_RECORDING),
        window: store_string(app, "shortcutWindow", DEFAULT_WINDOW),
        pause: store_string(app, "shortcutPause", DEFAULT_PAUSE),
    }
}

/// An empty string disables a shortcut; anything else must parse.
fn validate(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Ok(());
    }
    Shortcut::from_str(value)
        .map(|_| ())
        .map_err(|error| format!("'{value}' is not a valid shortcut: {error}"))
}

/// Reject configurations where two non-empty shortcuts collide.
///
/// Pure so it can be unit tested without a Tauri app handle.
fn check_no_duplicates(shortcuts: &[(&str, &str)]) -> Result<(), String> {
    for i in 0..shortcuts.len() {
        let (name_a, value_a) = shortcuts[i];
        if value_a.trim().is_empty() {
            continue;
        }
        for &(name_b, value_b) in &shortcuts[i + 1..] {
            if value_b.trim().is_empty() {
                continue;
            }
            if value_a == value_b {
                return Err(format!(
                    "'{value_a}' is assigned to both {name_a} and {name_b}; shortcuts must be unique"
                ));
            }
        }
    }
    Ok(())
}

/// (Re)register the shortcuts from settings.
///
/// Failures (for example when another app already owns the combination) are
/// logged rather than fatal so the rest of the app keeps working.
pub fn register<R: Runtime>(app: &AppHandle<R>) {
    let shortcuts = app.global_shortcut();
    let _ = shortcuts.unregister_all();

    let config = current(app);

    if !config.recording.trim().is_empty() {
        match shortcuts.on_shortcut(config.recording.as_str(), |app, _, event| {
            if event.state == ShortcutState::Pressed {
                crate::tray::toggle_recording_handler(app);
            }
        }) {
            Ok(()) => log::info!("Registered recording shortcut: {}", config.recording),
            Err(error) => log::warn!(
                "Failed to register recording shortcut '{}': {}",
                config.recording,
                error
            ),
        }
    }

    if !config.window.trim().is_empty() {
        match shortcuts.on_shortcut(config.window.as_str(), |app, _, event| {
            if event.state == ShortcutState::Pressed {
                crate::tray::toggle_main_window(app);
            }
        }) {
            Ok(()) => log::info!("Registered window shortcut: {}", config.window),
            Err(error) => log::warn!(
                "Failed to register window shortcut '{}': {}",
                config.window,
                error
            ),
        }
    }

    if !config.pause.trim().is_empty() {
        match shortcuts.on_shortcut(config.pause.as_str(), |app, _, event| {
            if event.state == ShortcutState::Pressed {
                crate::tray::toggle_pause_handler(app);
            }
        }) {
            Ok(()) => log::info!("Registered pause shortcut: {}", config.pause),
            Err(error) => log::warn!(
                "Failed to register pause shortcut '{}': {}",
                config.pause,
                error
            ),
        }
    }
}

#[tauri::command]
pub fn get_global_shortcuts<R: Runtime>(app: AppHandle<R>) -> serde_json::Value {
    let config = current(&app);
    serde_json::json!({
        "recording": config.recording,
        "window": config.window,
        "pause": config.pause,
    })
}

#[tauri::command]
pub fn set_global_shortcuts<R: Runtime>(
    app: AppHandle<R>,
    recording: String,
    window: String,
    pause: Option<String>,
) -> Result<(), String> {
    let recording = recording.trim().to_string();
    let window = window.trim().to_string();
    // `None` keeps whatever is already stored, so older callers that don't
    // know about the pause shortcut yet leave it untouched.
    let pause = pause
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| current(&app).pause);
    validate(&recording)?;
    validate(&window)?;
    validate(&pause)?;
    check_no_duplicates(&[
        ("recording", &recording),
        ("window", &window),
        ("pause", &pause),
    ])?;

    if let Ok(store) = app.store("store.json") {
        store.set("shortcutRecording", serde_json::Value::String(recording));
        store.set("shortcutWindow", serde_json::Value::String(window));
        store.set("shortcutPause", serde_json::Value::String(pause));
        let _ = store.save();
    }

    register(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_no_duplicates, validate, DEFAULT_PAUSE, DEFAULT_RECORDING, DEFAULT_WINDOW};
    use std::str::FromStr;
    use tauri_plugin_global_shortcut::Shortcut;

    #[test]
    fn default_shortcuts_parse() {
        assert!(Shortcut::from_str(DEFAULT_RECORDING).is_ok());
        assert!(Shortcut::from_str(DEFAULT_WINDOW).is_ok());
        assert!(Shortcut::from_str(DEFAULT_PAUSE).is_ok());
    }

    #[test]
    fn default_shortcuts_are_pairwise_distinct() {
        assert_ne!(DEFAULT_RECORDING, DEFAULT_WINDOW);
        assert_ne!(DEFAULT_RECORDING, DEFAULT_PAUSE);
        assert_ne!(DEFAULT_WINDOW, DEFAULT_PAUSE);
    }

    #[test]
    fn validation_allows_empty_and_rejects_garbage() {
        assert!(validate("").is_ok());
        assert!(validate("CmdOrCtrl+Shift+KeyR").is_ok());
        assert!(validate("Ctrl+Alt+Space").is_ok());
        assert!(validate("NotAKey").is_err());
    }

    #[test]
    fn duplicate_check_allows_defaults_and_empty_shortcuts() {
        assert!(check_no_duplicates(&[
            ("recording", DEFAULT_RECORDING),
            ("window", DEFAULT_WINDOW),
            ("pause", DEFAULT_PAUSE),
        ])
        .is_ok());
        assert!(check_no_duplicates(&[("recording", ""), ("window", ""), ("pause", ""),]).is_ok());
    }

    #[test]
    fn duplicate_check_rejects_matching_non_empty_shortcuts() {
        let result = check_no_duplicates(&[
            ("recording", "CmdOrCtrl+Shift+KeyR"),
            ("window", "CmdOrCtrl+Shift+KeyR"),
            ("pause", DEFAULT_PAUSE),
        ]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("CmdOrCtrl+Shift+KeyR"));
    }
}
