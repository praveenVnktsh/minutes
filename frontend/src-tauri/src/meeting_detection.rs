//! Identify a meeting app from the processes currently using system audio.
//!
//! Runs in Rust so the tray can update the moment audio starts, without waiting
//! on the webview (which macOS can throttle when the window is hidden).

/// Return the display name of a detected meeting app, or `None`.
///
/// Known apps win. Unknown apps fall back to the first non-media/browser app so
/// generic call apps still surface, mirroring the frontend allowlist.
pub fn detect_meeting_app(apps: &[String]) -> Option<String> {
    let mut generic: Option<String> = None;

    for raw in apps {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        let lower = name.to_lowercase();

        if let Some(display) = known_meeting_app(&lower) {
            return Some(display.to_string());
        }
        if generic.is_none() && !is_non_meeting_audio(&lower) {
            generic = Some(name.to_string());
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

fn is_non_meeting_audio(lower: &str) -> bool {
    if lower == "arc" {
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
    ];
    TOKENS.iter().any(|token| lower.contains(token))
}

#[cfg(test)]
mod tests {
    use super::detect_meeting_app;

    fn apps(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn recognizes_known_meeting_apps() {
        assert_eq!(
            detect_meeting_app(&apps(&["zoom.us"])).as_deref(),
            Some("Zoom")
        );
        assert_eq!(
            detect_meeting_app(&apps(&["Microsoft Teams (work or school)"])).as_deref(),
            Some("Microsoft Teams")
        );
        assert_eq!(
            detect_meeting_app(&apps(&["WhatsApp"])).as_deref(),
            Some("WhatsApp")
        );
    }

    #[test]
    fn ignores_media_and_browsers() {
        assert_eq!(detect_meeting_app(&apps(&["Spotify"])), None);
        assert_eq!(detect_meeting_app(&apps(&["Google Chrome"])), None);
        assert_eq!(detect_meeting_app(&apps(&["Minutes"])), None);
    }

    #[test]
    fn falls_back_to_unknown_apps() {
        assert_eq!(
            detect_meeting_app(&apps(&["SomeCallApp"])).as_deref(),
            Some("SomeCallApp")
        );
    }

    #[test]
    fn known_apps_win_over_the_fallback() {
        assert_eq!(
            detect_meeting_app(&apps(&["SomeCallApp", "zoom.us"])).as_deref(),
            Some("Zoom")
        );
    }
}
