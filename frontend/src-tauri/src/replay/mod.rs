//! Pipeline replay harness (developer tool).
//!
//! Feeds a recorded meeting through the real live pipeline (streaming live
//! transcription, the live summary timeline, Copilot with scripted asks), then
//! the final pass, diarization and the summary, recording every UI event to
//! `events.jsonl` and scoring the run against the live storyboard.
//!
//! - [`events`], [`script`] and [`report`] are pure and always compiled (their
//!   tests run in PR Check).
//! - [`runner`] drives the app headless on Tauri's mock runtime and is only
//!   built with the `dev-replay` feature:
//!
//! ```text
//! cargo run --release -p meetily --features dev-replay --example replay -- \
//!     --audio "/path/to/meeting/audio.mp4"
//! ```
//!
//! `serve` (`… --example replay -- serve`, `dev-replay` only) serves the
//! viewer and the runs on 127.0.0.1, with a live Copilot that answers about
//! any moment of a run: [`live_copilot`] rebuilds what Copilot knew then from
//! the events, [`live_ask`] asks it, and [`runs`] reads the run folders. Those
//! three are always compiled (pure, or the app's own Copilot), so release
//! builds carry no server and no embedded viewer.
//!
//! Outputs stay on this machine, under the app data folder's `replays/`.
//! Recordings are private: never commit or upload them, their event logs or
//! their reports. The viewer is `site/replay/`, which reads the files locally
//! in the browser.

pub mod events;
pub mod live_ask;
pub mod live_copilot;
pub mod report;
pub mod runs;
pub mod script;
pub mod trace;

/// The HTTP server and its embedded copy of `site/replay`: developer builds only.
#[cfg(feature = "dev-replay")]
pub mod serve;

use std::path::{Path, PathBuf};

/// llama-helper for a developer tool run from anywhere: next to the binary,
/// one folder up (`target/release` from `target/release/examples`), or in the
/// workspace's `target/release` / `target/debug`.
pub fn find_llama_helper(exe: Option<&Path>) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "llama-helper.exe"
    } else {
        "llama-helper"
    };
    let mut candidates = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        candidates.push(dir.join(name));
        if let Some(up) = dir.parent() {
            candidates.push(up.join(name));
        }
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for profile in ["release", "debug"] {
        candidates.push(workspace.join("target").join(profile).join(name));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Points the app's sidecar lookup at llama-helper (`MEETILY_LLAMA_HELPER`)
/// unless it is already set. Call before any thread starts.
pub fn use_local_llama_helper() -> Option<PathBuf> {
    if let Some(set) = std::env::var_os("MEETILY_LLAMA_HELPER").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(set));
    }
    let exe = std::env::current_exe().ok();
    let found = find_llama_helper(exe.as_deref())?;
    let found = found.canonicalize().unwrap_or(found);
    std::env::set_var("MEETILY_LLAMA_HELPER", &found);
    Some(found)
}

#[cfg(feature = "dev-replay")]
pub mod runner;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_helper_is_found_next_to_or_above_the_binary() {
        let target = tempfile::tempdir().unwrap();
        let examples = target.path().join("examples");
        std::fs::create_dir(&examples).unwrap();
        let name = if cfg!(windows) {
            "llama-helper.exe"
        } else {
            "llama-helper"
        };
        std::fs::write(target.path().join(name), "").unwrap();
        let found = find_llama_helper(Some(&examples.join("replay"))).unwrap();
        assert_eq!(found, target.path().join(name));
        std::fs::write(examples.join(name), "").unwrap();
        let found = find_llama_helper(Some(&examples.join("replay"))).unwrap();
        assert_eq!(found, examples.join(name), "next to the binary wins");
    }
}
