// macOS audio permissions handling
//
// What is left in this file opens a settings page and logs where the Audio
// Capture row lives. The one question it used to answer for itself — whether
// system audio is actually reaching the app — now belongs to
// `permission_check.rs`, which answers it from samples that arrived rather than
// from a tap that was constructed. Building a tap was never evidence: macOS
// hands a denied app a tap that opens cleanly and then yields silence, so the
// old check here passed for a user who would get nothing.

use anyhow::Result;
// Only the macOS paths log: off macOS these functions have nothing to say.
#[cfg(target_os = "macos")]
use log::{error, info};

use crate::audio::permission_check::{verify_system_audio, PermissionReport};

#[cfg(target_os = "macos")]
use std::process::Command;

/// Log where the Audio Capture permission (macOS 14.4+) lives. This reports
/// nothing about whether it was granted.
///
/// No reading is taken: it returns `true` on a Mac that has granted Audio
/// Capture and on one that has refused it. macOS exposes no way to query that
/// grant ahead of use — the prompt is raised by Core Audio itself, when
/// `AudioHardwareCreateProcessTap` builds the first tap — so the only honest
/// source of an answer is audio arriving, which
/// `permission_check::verify_system_audio` waits for. Callers that need to know
/// must ask that instead of reading anything into this `true`.
#[cfg(target_os = "macos")]
pub fn check_screen_recording_permission() -> bool {
    info!("ℹ️  Core Audio tap requires Audio Capture permission (macOS 14.4+)");
    info!("📍 The permission dialog appears when the first Core Audio tap is created");
    info!("   To review it: System Settings → Privacy & Security → Audio Capture");

    // Unconditional, for the reason given above: nothing here has been checked.
    true
}

/// The same non-answer off macOS, where no such permission exists.
#[cfg(not(target_os = "macos"))]
pub fn check_screen_recording_permission() -> bool {
    true // No Audio Capture permission on this platform, so nothing to report
}

/// Open Privacy & Security so the user can find the Audio Capture row.
///
/// Opening the page is the whole of it. macOS offers no URL that deep-links to
/// the Audio Capture pane and no API that asks for the grant directly, so the
/// user has to find the row themselves, and whether they then grant it is not
/// visible from here.
#[cfg(target_os = "macos")]
pub fn request_screen_recording_permission() -> Result<()> {
    info!("🔐 Opening System Settings for Audio Capture permission...");

    // Open System Settings to Privacy & Security page
    // Note: There's no direct URL for Audio Capture, so we open the main Privacy page
    let result = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security")
        .spawn();

    match result {
        Ok(_) => {
            // `open` was launched. Whether System Settings came to the front,
            // and what the user does once it is there, is not observed here.
            info!(
                "✅ Asked macOS to open System Settings - navigate to Privacy & Security → \
                 Audio Capture"
            );
            info!("👉 Enable Audio Capture there, then run the system audio check again");
            Ok(())
        }
        Err(e) => {
            error!("❌ Failed to open System Settings: {}", e);
            Err(anyhow::anyhow!("Failed to open System Settings: {}", e))
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_recording_permission() -> Result<()> {
    Ok(()) // Not required on other platforms
}

/// Tauri command to log where the Screen Recording (Audio Capture) setting is.
///
/// Registered in lib.rs, which is the only reason it still takes this shape;
/// no caller in this repository invokes it today. Its `true` means "logged",
/// not "granted" — see [`check_screen_recording_permission`].
#[tauri::command]
pub async fn check_screen_recording_permission_command() -> bool {
    check_screen_recording_permission()
}

/// Tauri command to open the settings page for the Screen Recording (Audio
/// Capture) permission. Returning `Ok` means the page was asked to open,
/// nothing about the grant.
#[tauri::command]
pub async fn request_screen_recording_permission_command() -> Result<(), String> {
    request_screen_recording_permission().map_err(|e| e.to_string())
}

/// Tauri command behind onboarding's system audio row.
///
/// It decides nothing itself, deliberately: onboarding and the start of a
/// recording both ask `verify_system_audio`, so the two surfaces cannot reach
/// different conclusions about the same tap — which is exactly what they used
/// to do, onboarding passing a tap that recording would find silent. Creating
/// that tap is still what raises the macOS prompt, so the prompt and the
/// verification remain one act; it is the verdict that now comes from samples.
///
/// The `Result` exists for the frontend's invoke signature. A check that could
/// not be made comes back as an `Undetermined` verdict rather than an error, so
/// nothing here returns `Err`.
#[tauri::command]
pub async fn trigger_system_audio_permission_command() -> Result<PermissionReport, String> {
    Ok(verify_system_audio().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_recording_check_answers_the_same_on_every_machine() {
        // It returns true whether or not Audio Capture was granted, which is
        // why no caller may read it as evidence of a grant. Pinned here so a
        // change that gives it a real reading has to come through this test and
        // say what it now checks.
        assert!(check_screen_recording_permission());
    }

    /// Off macOS the app captures no system audio at all, so onboarding must
    /// not be handed a grant for it. The macOS path is left to a machine with a
    /// tap: CI runs `cargo test -p meetily` on headless Ubuntu and nothing here
    /// may open a device.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn the_onboarding_command_claims_no_grant_off_macos() {
        use crate::audio::permission_check::PermissionVerdict;

        let report = trigger_system_audio_permission_command()
            .await
            .expect("the system audio check reports a verdict rather than an error");

        assert_eq!(report.verdict, PermissionVerdict::Undetermined);
        assert!(!report.is_authorized());
        assert!(!report.is_denied());
    }
}
