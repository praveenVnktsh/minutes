//! What Copilot knew at a moment of a replay, rebuilt from the run's
//! `events.jsonl`. Pure.
//!
//! The viewer's live Copilot asks "as if at this moment of the meeting": the
//! finalized transcript lines Copilot's listener had received, the live
//! summary's latest snapshot and the user's notes, all as of a cursor. They
//! are read back from the events the replay recorded (the same
//! `transcript-update` and `live-timeline-update` payloads the app's Copilot
//! hears), never re-transcribed.

use serde_json::Value;

use crate::copilot::context::{self, Line, TimelineItem};

/// Where the cursor is: before record `seq` when given (exact, what the
/// viewer's cursor is), else every record at or before meeting time
/// `audio_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cutoff {
    pub audio_ms: u64,
    pub seq: Option<u64>,
}

impl Cutoff {
    fn includes(&self, seq: u64, audio_ms: u64) -> bool {
        match self.seq {
            Some(n) => seq < n,
            None => audio_ms <= self.audio_ms,
        }
    }
}

/// Copilot's inputs at a cutoff.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inputs {
    /// Final lines, kept the way Copilot keeps them (`context::push_line`).
    pub lines: Vec<Line>,
    /// The live summary's latest snapshot.
    pub timeline: Vec<TimelineItem>,
    /// The user's notes as last fed (runs recorded before notes text was
    /// logged have none).
    pub notes: String,
}

impl Inputs {
    /// The end of the newest line: the moment an answer is "as of".
    pub fn latest_ms(&self) -> i64 {
        self.lines.iter().map(|l| l.end_ms).max().unwrap_or(0)
    }
}

const TRANSCRIPT: &str = "\"name\":\"transcript-update\"";
const TIMELINE: &str = "\"name\":\"live-timeline-update\"";
const SCRIPT: &str = "\"name\":\"script\"";

fn timeline_items(payload: &Value) -> Vec<TimelineItem> {
    payload["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| TimelineItem {
            start_ms: entry["start_ms"].as_i64().unwrap_or(0),
            kind: entry["kind"].as_str().unwrap_or("point").to_string(),
            text: entry["text"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// Copilot's inputs at `cutoff`, from an `events.jsonl` body.
pub fn inputs_at(events_jsonl: &str, cutoff: Cutoff) -> Inputs {
    let mut inputs = Inputs::default();
    for line in events_jsonl.lines() {
        // Most records are logs and measurements: skip them unparsed.
        let wanted = line.contains(TRANSCRIPT) || line.contains(TIMELINE) || line.contains(SCRIPT);
        if !wanted {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let seq = record["seq"].as_u64().unwrap_or(0);
        let audio_ms = record["audio_ms"].as_u64().unwrap_or(0);
        if !cutoff.includes(seq, audio_ms) {
            // Records are in write order and meeting time never goes back.
            break;
        }
        let payload = &record["payload"];
        match (record["type"].as_str(), record["name"].as_str()) {
            (Some("event"), Some("transcript-update")) => {
                if let Some(line) = crate::copilot::line_from_event(&payload.to_string()) {
                    context::push_line(&mut inputs.lines, line);
                }
            }
            (Some("event"), Some("live-timeline-update")) => {
                inputs.timeline = timeline_items(payload);
            }
            (Some("debug"), Some("script")) if payload["kind"] == "notes" => {
                if let Some(text) = payload["text"].as_str() {
                    inputs.notes = text.to_string();
                }
            }
            _ => {}
        }
    }
    inputs
}

/// For each `copilot-state` snapshot that opened a new question, what the
/// rebuilt inputs say vs. what Copilot had: `(seq, its line count, ours, its
/// "as of" ms, ours)`. They match when the rebuild is faithful.
pub fn compare_with_recorded_asks(events_jsonl: &str) -> Vec<(u64, usize, usize, i64, i64)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in events_jsonl.lines() {
        if !line.contains("\"name\":\"copilot-state\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &record["payload"];
        let Some(pending) = payload["messages"].as_array().and_then(|messages| {
            messages
                .iter()
                .find(|m| m["status"] == "pending" && m["kind"] != "suggestion")
        }) else {
            continue;
        };
        let id = pending["id"].as_str().unwrap_or_default().to_string();
        if !seen.insert(id) {
            continue;
        }
        let seq = record["seq"].as_u64().unwrap_or(0);
        let inputs = inputs_at(
            events_jsonl,
            Cutoff {
                audio_ms: 0,
                seq: Some(seq),
            },
        );
        out.push((
            seq,
            payload["lineCount"].as_u64().unwrap_or(0) as usize,
            inputs.lines.len(),
            pending["audioMs"].as_i64().unwrap_or(0),
            inputs.latest_ms(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copilot::context::Speaker;

    fn record(seq: u64, audio_ms: u64, kind: &str, name: &str, payload: Value) -> String {
        serde_json::json!({
            "seq": seq, "t_ms": seq * 10, "audio_ms": audio_ms,
            "type": kind, "name": name, "payload": payload,
        })
        .to_string()
    }

    fn transcript(
        seq: u64,
        at: u64,
        start_s: f64,
        source: &str,
        text: &str,
        partial: bool,
    ) -> String {
        record(
            seq,
            at,
            "event",
            "transcript-update",
            serde_json::json!({
                "text": text, "source": source, "is_partial": partial,
                "audio_start_time": start_s, "audio_end_time": start_s + 4.0,
            }),
        )
    }

    fn timeline(seq: u64, at: u64, texts: &[(&str, i64)]) -> String {
        let entries: Vec<Value> = texts
            .iter()
            .map(|(text, start)| serde_json::json!({"start_ms": start, "kind": "point", "text": text}))
            .collect();
        record(
            seq,
            at,
            "event",
            "live-timeline-update",
            serde_json::json!({"entries": entries, "status": "listening"}),
        )
    }

    fn meeting() -> String {
        [
            record(0, 0, "meta", "run-start", serde_json::json!({})),
            transcript(1, 6_000, 1.0, "system", "Hello there", false),
            transcript(2, 9_000, 5.0, "system", "Can y", true),
            record(3, 9_500, "debug", "log", serde_json::json!({"message": "\"name\":\"script\""})),
            transcript(4, 12_000, 5.0, "mic", "Can you hear me?", false),
            timeline(5, 15_000, &[("Greetings", 1_000)]),
            record(6, 16_000, "debug", "script", serde_json::json!({"kind": "notes", "count": 1, "text": "[00:10]\nask about budget"})),
            record(7, 18_000, "debug", "copilot-state", serde_json::json!({
                "lineCount": 2,
                "messages": [{"id": "a", "kind": "ask", "status": "pending", "audioMs": 9_000}],
            })),
            transcript(8, 30_000, 20.0, "system", "What is the budget?", false),
            timeline(9, 31_000, &[("Greetings", 1_000), ("Budget asked", 20_000)]),
            record(10, 32_000, "debug", "copilot-state", serde_json::json!({
                "lineCount": 3,
                "messages": [
                    {"id": "a", "kind": "ask", "status": "done", "audioMs": 9_000},
                    {"id": "b", "kind": "action", "status": "pending", "audioMs": 24_000},
                ],
            })),
        ]
        .join("\n")
    }

    #[test]
    fn the_inputs_are_what_copilot_had_heard_by_the_cursor() {
        let events = meeting();
        let early = inputs_at(
            &events,
            Cutoff {
                audio_ms: 12_000,
                seq: None,
            },
        );
        assert_eq!(early.lines.len(), 2, "partials are not Copilot lines");
        assert_eq!(early.lines[0].speaker, Speaker::Others);
        assert_eq!(early.lines[1].speaker, Speaker::You);
        assert!(early.timeline.is_empty());
        assert_eq!(early.notes, "");
        assert_eq!(early.latest_ms(), 9_000);

        let later = inputs_at(
            &events,
            Cutoff {
                audio_ms: 16_000,
                seq: None,
            },
        );
        assert_eq!(later.timeline.len(), 1);
        assert_eq!(later.notes, "[00:10]\nask about budget");

        let end = inputs_at(
            &events,
            Cutoff {
                audio_ms: u64::MAX,
                seq: None,
            },
        );
        assert_eq!(end.lines.len(), 3);
        assert_eq!(
            end.timeline.len(),
            2,
            "the newest live summary snapshot wins"
        );
        assert_eq!(end.timeline[1].text, "Budget asked");
        assert_eq!(end.latest_ms(), 24_000);
    }

    #[test]
    fn a_record_cursor_is_exact() {
        let events = meeting();
        let before = inputs_at(
            &events,
            Cutoff {
                audio_ms: 0,
                seq: Some(8),
            },
        );
        assert_eq!(before.lines.len(), 2);
        let after = inputs_at(
            &events,
            Cutoff {
                audio_ms: 0,
                seq: Some(9),
            },
        );
        assert_eq!(after.lines.len(), 3);
    }

    #[test]
    fn the_rebuild_matches_what_the_live_copilot_saw() {
        let compared = compare_with_recorded_asks(&meeting());
        assert_eq!(compared.len(), 2);
        for (seq, recorded_lines, rebuilt_lines, recorded_ms, rebuilt_ms) in compared {
            assert_eq!(recorded_lines, rebuilt_lines, "line count at record {seq}");
            assert_eq!(recorded_ms, rebuilt_ms, "as-of time at record {seq}");
        }
    }

    /// Against a real run: `REPLAY_EVENTS=/path/to/events.jsonl cargo test
    /// -p meetily --lib replay::live_copilot -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_rebuild_matches_a_real_run() {
        let Ok(path) = std::env::var("REPLAY_EVENTS") else {
            eprintln!("set REPLAY_EVENTS");
            return;
        };
        let events = std::fs::read_to_string(path).unwrap();
        let compared = compare_with_recorded_asks(&events);
        assert!(!compared.is_empty());
        for row in compared {
            eprintln!("{row:?}");
            assert_eq!(row.1, row.2);
            assert_eq!(row.3, row.4);
        }
    }
}
