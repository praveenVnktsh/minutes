//! The replay event log (`events.jsonl`): one JSON object per line.
//!
//! ```json
//! {"seq":12,"t_ms":4810,"audio_ms":5400,"type":"event","name":"transcript-update","payload":{…}}
//! ```
//!
//! - `seq`: 0-based, in write order.
//! - `t_ms`: wall-clock milliseconds since the replay started.
//! - `audio_ms`: how much of the recording had been fed by then.
//! - `type`: `meta` (run header/footer), `event` (a Tauri event the app's UI
//!   listens to, payload exactly as emitted) or `debug` (harness measurements:
//!   see [`debug`]).
//!
//! The viewer (`site/replay/`) reads the same format; its README documents it
//! for people. Keep the two in step.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    Meta,
    Event,
    Debug,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub t_ms: u64,
    pub audio_ms: u64,
    #[serde(rename = "type")]
    pub kind: RecordType,
    pub name: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl Record {
    pub fn is_event(&self, name: &str) -> bool {
        self.kind == RecordType::Event && self.name == name
    }

    pub fn is_debug(&self, name: &str) -> bool {
        self.kind == RecordType::Debug && self.name == name
    }

    pub fn to_line(&self) -> String {
        // A Record always serializes: its payload is already a Value.
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Names of `debug` records.
pub mod debug {
    /// `{phase}`: live-start, audio-end, stop, stopped, final-pass-start,
    /// final-pass-done, summary-start, summary-done, run-end.
    pub const PHASE: &str = "phase";
    /// A scripted Copilot action: `{at_ms, kind, text, tag, message_id, error}`.
    pub const SCRIPT: &str = "script";
    /// A detector decision: `{counter, decision, line_ms}`.
    pub const DETECTOR: &str = "detector";
    /// A finished Copilot request: `{id, kind, tag, first_token_ms, total_ms,
    /// tokens_per_second, prompt_tokens, reused_tokens, status, error,
    /// first_delta_ms}`.
    pub const COPILOT_REQUEST: &str = "copilot-request";
    /// llama-helper queue activity from the app's log: `{what, message}`.
    pub const SIDECAR: &str = "sidecar";
    /// Live transcription health, every second: `{backlog_s, rtf, rtf_total,
    /// decoded_audio_s}`.
    pub const LIVE: &str = "live";
    /// `{app_rss_mb, helper_rss_mb}` every 5 s.
    pub const MEMORY: &str = "memory";
    /// The final transcript after the final pass and diarization:
    /// `{lines: [{start, end, speaker, text}]}`.
    pub const FINAL_TRANSCRIPT: &str = "final-transcript";
    /// One summary run: `{run, status, markdown, ms, error}`.
    pub const SUMMARY: &str = "summary";
    /// A warning or error the app logged: `{level, target, message}`.
    pub const LOG: &str = "log";
}

/// Tauri events the recording screen and the post-meeting flow listen to.
pub const UI_EVENTS: &[&str] = &[
    "transcript-update",
    "transcript-partial",
    "live-timeline-update",
    "copilot-state",
    "copilot-delta",
    "transcription-error",
    "transcription-warning",
    "transcription-queue-status",
    "transcription-queue-progress",
    "transcription-queue-complete",
    "transcription-queue-error",
    "retranscription-progress",
    "retranscription-complete",
    "retranscription-error",
    "final-transcript-pass-complete",
    "diarization-progress",
    "diarization-status-changed",
    "diarization-complete",
    "meeting-activity-updated",
    "transcripts-updated",
];

/// Parse an `events.jsonl` body. Blank lines are skipped; a malformed line is
/// an error naming its line number.
pub fn parse_jsonl(text: &str) -> anyhow::Result<Vec<Record>> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Record>(line)
                .map_err(|e| anyhow::anyhow!("events.jsonl line {}: {e}", index + 1))
        })
        .collect()
}

/// `[MM:SS]` for a time in seconds (`[H:MM:SS]` from an hour), as Copilot cites.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).floor() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Every `[MM:SS]` / `[H:MM:SS]` citation in `text`, in seconds.
pub fn citations(text: &str) -> Vec<u64> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end) = text[i + 1..].find(']') {
                let inner = &text[i + 1..i + 1 + end];
                if let Some(seconds) = parse_clock(inner) {
                    found.push(seconds);
                }
                i += end + 1;
                continue;
            }
        }
        i += 1;
    }
    found
}

/// `MM:SS` or `H:MM:SS` to seconds; `None` for anything else.
pub fn parse_clock(value: &str) -> Option<u64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|p| p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let numbers: Vec<u64> = parts.iter().map(|p| p.parse().unwrap_or(0)).collect();
    let (h, m, s) = match numbers.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    if s >= 60 || (parts.len() == 3 && m >= 60) {
        return None;
    }
    Some(h * 3600 + m * 60 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_through_jsonl() {
        let records = vec![
            Record {
                seq: 0,
                t_ms: 0,
                audio_ms: 0,
                kind: RecordType::Meta,
                name: "run-start".into(),
                payload: serde_json::json!({"schema_version": SCHEMA_VERSION}),
            },
            Record {
                seq: 1,
                t_ms: 4810,
                audio_ms: 5400,
                kind: RecordType::Event,
                name: "transcript-update".into(),
                payload: serde_json::json!({"text": "Hello", "source": "system"}),
            },
        ];
        let body: String = records.iter().map(|r| r.to_line() + "\n").collect();
        assert!(body.contains("\"type\":\"event\""));
        let parsed = parse_jsonl(&format!("{body}\n\n")).unwrap();
        assert_eq!(parsed, records);
        assert!(parsed[1].is_event("transcript-update"));
        assert!(!parsed[1].is_debug("transcript-update"));
    }

    #[test]
    fn a_bad_line_names_its_line_number() {
        let error = parse_jsonl("{\"seq\":0,\"t_ms\":0,\"audio_ms\":0,\"type\":\"meta\",\"name\":\"x\"}\nnot json")
            .unwrap_err();
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    #[test]
    fn citations_find_minute_and_hour_forms_only() {
        assert_eq!(
            citations("At [05:07] and [1:02:03], not [5] or [ab:cd] or [12:75]."),
            vec![307, 3723]
        );
        assert_eq!(citations("[00:00]"), vec![0]);
        assert!(citations("no brackets [").is_empty());
    }

    #[test]
    fn clock_matches_copilot_format() {
        assert_eq!(clock(65.9), "01:05");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(-3.0), "00:00");
    }
}
