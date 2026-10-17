//! The replay runs on this machine, for both viewers (`replay serve` and the
//! app's Replays page): their folders, their summaries, their recordings and
//! the checked live asks. Pure file reads, always compiled.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::{json, Value};

pub use super::live_copilot::Cutoff;
use crate::copilot::prompt::QuickAction;

/// The longest question accepted.
const MAX_QUESTION_CHARS: usize = 2_000;

/// A run folder name: what the recorder makes (`20261016-162607-m35`).
pub fn valid_run_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The folder of run `name`, only if it is a directory directly under `root`.
pub fn run_dir(root: &Path, name: &str) -> Option<PathBuf> {
    if !valid_run_name(name) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let dir = root.join(name).canonicalize().ok()?;
    (dir.parent() == Some(root.as_path()) && dir.is_dir()).then_some(dir)
}

fn first_line(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    // The header is in the first few records (after an activity update).
    let mut reader = BufReader::new(file);
    for _ in 0..8 {
        line.clear();
        reader.read_line(&mut line).ok()?;
        let record: Value = serde_json::from_str(&line).ok()?;
        if record["name"] == "run-start" {
            return Some(record);
        }
    }
    None
}

/// The meeting folder's name: the recording is linked in the sandbox.
fn recording_name(dir: &Path) -> Option<String> {
    let meeting = dir.join("sandbox").join("meeting");
    std::fs::read_dir(meeting)
        .ok()?
        .flatten()
        .find_map(|entry| std::fs::read_link(entry.path()).ok())
        .and_then(|target| {
            target
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
        })
}

/// One run's entry in `/api/runs`.
fn run_summary(dir: &Path) -> Option<Value> {
    let name = dir.file_name()?.to_str()?.to_string();
    let events = dir.join("events.jsonl");
    let meta = events.metadata().ok()?;
    let header = first_line(&events)
        .map(|r| r["payload"].clone())
        .unwrap_or(Value::Null);
    let report: Option<Value> = std::fs::read_to_string(dir.join("report.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let fresh = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age < Duration::from_secs(60));
    let status = if report.is_some() {
        "done"
    } else if fresh {
        "running"
    } else {
        "incomplete"
    };
    let count = |status: &str| {
        report
            .as_ref()
            .and_then(|r| r["criteria"].as_array())
            .map(|c| c.iter().filter(|c| c["status"] == status).count())
    };
    let interactive = std::fs::read_to_string(dir.join("interactive.jsonl"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    Some(json!({
        "name": name,
        "status": status,
        "recording": recording_name(dir),
        "audio_name": header["audio_name"],
        "duration_s": header["duration_s"],
        "realtime": header["realtime"],
        "started_at": header["started_at"],
        "events_bytes": meta.len(),
        "passed": count("pass"),
        "failed": count("fail"),
        "interactive": interactive,
    }))
}

/// The runs under `root`, newest first: what `GET /api/runs` answers.
pub fn list_runs(root: &Path) -> Value {
    let mut runs: Vec<Value> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| e.file_name().to_str().is_some_and(valid_run_name))
        .filter_map(|e| run_summary(&e.path()))
        .collect();
    runs.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    json!({ "runs": runs })
}

/// Audio files a run may link, and their types.
const AUDIO_TYPES: &[(&str, &str)] = &[
    ("mp4", "audio/mp4"),
    ("m4a", "audio/mp4"),
    ("wav", "audio/wav"),
    ("mp3", "audio/mpeg"),
    ("webm", "audio/webm"),
    ("ogg", "audio/ogg"),
];

/// The recording a run replayed: the `audio.<ext>` the recorder linked into
/// its sandbox's meeting folder, and nothing else. `Err` names the missing
/// file when the link no longer resolves.
pub fn run_audio(dir: &Path) -> Result<(PathBuf, &'static str), String> {
    let meeting = dir.join("sandbox").join("meeting");
    let entries =
        std::fs::read_dir(&meeting).map_err(|_| "This run has no linked recording".to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(ext) = name.strip_prefix("audio.") else {
            continue;
        };
        let Some((_, kind)) = AUDIO_TYPES
            .iter()
            .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        else {
            continue;
        };
        let link = entry.path();
        return match link.canonicalize() {
            Ok(file) if file.is_file() => Ok((file, kind)),
            _ => Err(format!(
                "The recording is not on this machine any more: {}",
                std::fs::read_link(&link).unwrap_or(link).display()
            )),
        };
    }
    Err("This run has no linked recording".to_string())
}

/// `POST /api/runs/<run>/copilot`.
#[derive(Debug, Deserialize)]
pub struct AskBody {
    /// Meeting time of the cursor.
    pub audio_ms: u64,
    /// The cursor as a record count (exact); `audio_ms` alone otherwise.
    #[serde(default)]
    pub seq: Option<u64>,
    /// `ask` (a question) or `action` (a quick action).
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

/// A checked ask: the quick action or the question.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub cutoff: Cutoff,
    pub action: Option<QuickAction>,
    pub question: Option<String>,
}

pub fn parse_ask(body: &[u8]) -> Result<Ask, String> {
    let body: AskBody =
        serde_json::from_slice(body).map_err(|e| format!("Bad request body: {e}"))?;
    let cutoff = Cutoff {
        audio_ms: body.audio_ms,
        seq: body.seq,
    };
    match body.kind.as_str() {
        "ask" => {
            let text = body.text.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return Err("Type a question first".into());
            }
            if text.chars().count() > MAX_QUESTION_CHARS {
                return Err(format!(
                    "Questions are limited to {MAX_QUESTION_CHARS} characters"
                ));
            }
            Ok(Ask {
                cutoff,
                action: None,
                question: Some(text),
            })
        }
        "action" => {
            let action = body
                .action
                .as_deref()
                .and_then(QuickAction::parse)
                .filter(|a| *a != QuickAction::Shorter)
                .ok_or("Unknown quick action")?;
            Ok(Ask {
                cutoff,
                action: Some(action),
                question: None,
            })
        }
        _ => Err("kind must be \"ask\" or \"action\"".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_names_and_folders_stay_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("20261016-162607-m35")).unwrap();
        std::fs::write(root.path().join("file.txt"), "x").unwrap();
        assert!(run_dir(root.path(), "20261016-162607-m35").is_some());
        for bad in [
            "", "..", "../etc", "a/b", ".hidden", "file.txt", "missing", "%2e%2e", "..%2f",
        ] {
            assert!(run_dir(root.path(), bad).is_none(), "{bad}");
        }
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
            assert!(
                run_dir(root.path(), "link").is_none(),
                "a symlink out of the root"
            );
        }
    }

    #[test]
    fn runs_are_listed_with_their_status() {
        let root = tempfile::tempdir().unwrap();
        let done = root.path().join("20261016-100000-a");
        std::fs::create_dir(&done).unwrap();
        std::fs::write(
            done.join("events.jsonl"),
            "{\"seq\":0,\"t_ms\":0,\"audio_ms\":0,\"type\":\"event\",\"name\":\"meeting-activity-updated\",\"payload\":{}}\n\
             {\"seq\":1,\"t_ms\":0,\"audio_ms\":0,\"type\":\"meta\",\"name\":\"run-start\",\"payload\":{\"audio_name\":\"audio.mp4\",\"duration_s\":60}}\n",
        )
        .unwrap();
        std::fs::write(
            done.join("report.json"),
            "{\"criteria\":[{\"status\":\"pass\"},{\"status\":\"fail\"},{\"status\":\"pass\"}]}",
        )
        .unwrap();
        std::fs::write(done.join("interactive.jsonl"), "{}\n{}\n").unwrap();
        let running = root.path().join("20261016-110000-b");
        std::fs::create_dir(&running).unwrap();
        std::fs::write(running.join("events.jsonl"), "").unwrap();
        std::fs::create_dir(root.path().join("no-events")).unwrap();
        let list = list_runs(root.path());
        let runs = list["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["name"], "20261016-110000-b");
        assert_eq!(runs[0]["status"], "running");
        assert_eq!(runs[1]["status"], "done");
        assert_eq!(runs[1]["passed"], 2);
        assert_eq!(runs[1]["failed"], 1);
        assert_eq!(runs[1]["duration_s"], 60);
        assert_eq!(runs[1]["interactive"], 2);
    }

    #[test]
    fn asks_are_checked() {
        let ask = parse_ask(br#"{"audio_ms":720000,"seq":3000,"kind":"ask","text":"  What are they asking me for?  "}"#).unwrap();
        assert_eq!(
            ask.question.as_deref(),
            Some("What are they asking me for?")
        );
        assert_eq!(
            ask.cutoff,
            Cutoff {
                audio_ms: 720_000,
                seq: Some(3000)
            }
        );
        let action = parse_ask(br#"{"audio_ms":1,"kind":"action","action":"catch_up"}"#).unwrap();
        assert_eq!(action.action, Some(QuickAction::CatchUp));
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"ask","text":"  "}"#).is_err());
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"action","action":"shorter"}"#).is_err());
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"action","action":"nope"}"#).is_err());
        assert!(parse_ask(br#"{"audio_ms":1,"kind":"other"}"#).is_err());
        let long = format!(
            r#"{{"audio_ms":1,"kind":"ask","text":"{}"}}"#,
            "a".repeat(2_001)
        );
        assert!(parse_ask(long.as_bytes()).is_err());
    }

    #[test]
    fn a_run_s_recording_is_its_linked_audio_file_only() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20261016-100000-a");
        let meeting = run.join("sandbox").join("meeting");
        std::fs::create_dir_all(&meeting).unwrap();
        assert!(run_audio(&run).unwrap_err().contains("no linked recording"));
        let recording = tempfile::tempdir().unwrap();
        let source = recording.path().join("audio.m4a");
        std::fs::write(&source, b"x").unwrap();
        std::fs::write(meeting.join("other.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, meeting.join("audio.m4a")).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&source, meeting.join("audio.m4a")).unwrap();
        let (path, kind) = run_audio(&run).unwrap();
        assert_eq!(path, source.canonicalize().unwrap());
        assert_eq!(kind, "audio/mp4");
    }
}
