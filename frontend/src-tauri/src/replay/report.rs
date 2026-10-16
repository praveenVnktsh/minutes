//! Scores a replay's event log against the live storyboard's pass criteria
//! (docs/superpowers/specs/2026-10-16-live-copilot-design.md, "Pass criteria").
//!
//! Pure: everything is computed from the records, so a run can be re-scored
//! later (`replay --rescore <events.jsonl>`), and the viewer shows the same
//! numbers.
//!
//! The markdown report holds counts and timestamps only; meeting text (the
//! lines the detector fired on, answers) is added only with `verbose`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use super::events::{citations, clock, debug, Record, RecordType};

pub const FIRST_WORDS_LIMIT_MS: u64 = 3_000;
pub const CARD_LATENCY_LIMIT_MS: u64 = 3_000;
pub const CARD_COOLDOWN_MS: u64 = 20_000;
pub const RTF_LIMIT: f64 = 0.9;
/// Rolling window for the real-time factor.
pub const RTF_WINDOW_MS: u64 = 60_000;
/// Copilot activity this long after stop still counts as "during" it
/// (cancellation snapshots).
pub const AFTER_END_GRACE_MS: u64 = 2_000;
/// Citations are whole seconds; a line counts if it is this close.
pub const CITATION_TOLERANCE_S: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    NotApplicable,
    Info,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::NotApplicable => "n/a",
            Status::Info => "info",
        }
    }

    fn from_bool(ok: bool) -> Self {
        if ok {
            Status::Pass
        } else {
            Status::Fail
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Criterion {
    pub id: &'static str,
    pub label: &'static str,
    pub status: Status,
    /// The key number(s), short.
    pub value: String,
    /// Timestamps and counts behind the value (no meeting text).
    pub detail: Vec<String>,
    /// Meeting text behind the value; only in the verbose markdown.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub private_detail: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CopilotRequest {
    pub id: String,
    pub kind: String,
    pub tag: Option<String>,
    pub audio_ms: i64,
    pub status: String,
    pub first_token_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub tokens_per_second: Option<f64>,
    pub prompt_tokens: Option<u64>,
    pub reused_tokens: Option<u64>,
    pub deltas: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub meta: Value,
    pub criteria: Vec<Criterion>,
    pub requests: Vec<CopilotRequest>,
    pub stats: BTreeMap<String, Value>,
}

impl Report {
    pub fn passed(&self) -> usize {
        self.criteria
            .iter()
            .filter(|c| c.status == Status::Pass)
            .count()
    }

    pub fn failed(&self) -> usize {
        self.criteria
            .iter()
            .filter(|c| c.status == Status::Fail)
            .count()
    }
}

fn num(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn uint(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn at(ms: u64) -> String {
    clock(ms as f64 / 1000.0)
}

/// A finalized live line.
#[derive(Debug, Clone)]
struct LiveLine {
    t_ms: u64,
    source: String,
    start_s: f64,
    text: String,
}

#[derive(Debug, Clone)]
struct FinalLine {
    start: f64,
    end: f64,
}

fn live_lines(records: &[Record]) -> Vec<LiveLine> {
    records
        .iter()
        .filter(|r| r.is_event("transcript-update"))
        .filter(|r| {
            !r.payload
                .get("is_partial")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|r| {
            let text = text(&r.payload, "text")?.trim().to_string();
            (!text.is_empty()).then(|| LiveLine {
                t_ms: r.t_ms,
                source: text_or(&r.payload, "source"),
                start_s: num(&r.payload, "audio_start_time").unwrap_or(0.0),
                text,
            })
        })
        .collect()
}

/// A sidecar record's kind: `phase` from the queue trace, `what` from the log.
fn sidecar_what(payload: &Value) -> String {
    text(payload, "phase")
        .or_else(|| text(payload, "what"))
        .unwrap_or_default()
        .to_string()
}

fn text_or(value: &Value, key: &str) -> String {
    text(value, key).unwrap_or_default().to_string()
}

fn final_lines(records: &[Record]) -> Option<Vec<FinalLine>> {
    let record = records
        .iter()
        .rev()
        .find(|r| r.is_debug(debug::FINAL_TRANSCRIPT))?;
    Some(
        record
            .payload
            .get("lines")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|l| {
                        let start = num(l, "start")?;
                        Some(FinalLine {
                            start,
                            end: num(l, "end").unwrap_or(start).max(start),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn phase_t(records: &[Record], phase: &str) -> Option<u64> {
    records
        .iter()
        .find(|r| r.is_debug(debug::PHASE) && text(&r.payload, "phase") == Some(phase))
        .map(|r| r.t_ms)
}

/// Every Copilot message as last seen, with when it first appeared.
struct Messages {
    last: Vec<Value>,
    first_seen: HashMap<String, u64>,
    /// Meeting time (audio_ms) each message first appeared at.
    first_seen_audio: HashMap<String, u64>,
    deltas: HashMap<String, Vec<u64>>,
}

fn messages(records: &[Record]) -> Messages {
    let mut by_id: Vec<(String, Value)> = Vec::new();
    let mut first_seen = HashMap::new();
    let mut first_seen_audio = HashMap::new();
    let mut deltas: HashMap<String, Vec<u64>> = HashMap::new();
    for record in records {
        if record.is_event("copilot-state") {
            let Some(list) = record.payload.get("messages").and_then(Value::as_array) else {
                continue;
            };
            for message in list {
                let Some(id) = text(message, "id") else {
                    continue;
                };
                first_seen.entry(id.to_string()).or_insert(record.t_ms);
                first_seen_audio
                    .entry(id.to_string())
                    .or_insert(record.audio_ms);
                match by_id.iter_mut().find(|(known, _)| known == id) {
                    Some((_, slot)) => *slot = message.clone(),
                    None => by_id.push((id.to_string(), message.clone())),
                }
            }
        } else if record.is_event("copilot-delta") {
            if let Some(id) = text(&record.payload, "id") {
                deltas.entry(id.to_string()).or_default().push(record.t_ms);
            }
        }
    }
    Messages {
        last: by_id.into_iter().map(|(_, m)| m).collect(),
        first_seen,
        first_seen_audio,
        deltas,
    }
}

fn honest(reply: &str) -> bool {
    let reply = reply.to_lowercase().replace('’', "'");
    [
        "not come up",
        "hasn't come up",
        "has not come up",
        "didn't come up",
        "did not come up",
        "hasn't been discussed",
        "has not been discussed",
        "wasn't discussed",
        "was not discussed",
        "not been discussed",
        "not discussed",
        "not been mentioned",
        "wasn't mentioned",
        "was not mentioned",
        "no mention",
        "not mentioned",
        "isn't mentioned",
        "is not mentioned",
        "not covered",
        "no discussion",
        "nothing about",
        "nothing was said",
        "hasn't been mentioned",
        "doesn't cover",
        "does not cover",
        "don't cover",
        "do not cover",
    ]
    .iter()
    .any(|phrase| reply.contains(phrase))
}

/// Real-time factor of live transcription (decode time over audio time) for
/// each `RTF_WINDOW_MS` of decoded audio, from the cumulative `busy_s` /
/// `decoded_audio_s` of `live` records. Works for real-time and unpaced runs.
fn rtf_windows(records: &[Record]) -> Vec<(u64, f64)> {
    let samples = live_samples(records);
    let window = RTF_WINDOW_MS as f64 / 1000.0;
    let mut out = Vec::new();
    let mut start = 0;
    for (index, &(t, busy, audio, _)) in samples.iter().enumerate() {
        while samples[start].2 + window < audio {
            start += 1;
        }
        if start == index {
            continue;
        }
        let (_, busy0, audio0, _) = samples[start];
        let audio_delta = audio - audio0;
        if audio_delta >= window / 2.0 {
            out.push((t, (busy - busy0) / audio_delta));
        }
    }
    out
}

/// `(t_ms, busy_s, decoded_audio_s, llm_busy)` of every `live` record.
fn live_samples(records: &[Record]) -> Vec<(u64, f64, f64, bool)> {
    records
        .iter()
        .filter(|r| r.is_debug(debug::LIVE))
        .filter_map(|r| {
            Some((
                r.t_ms,
                num(&r.payload, "busy_s")?,
                num(&r.payload, "decoded_audio_s")?,
                r.payload
                    .get("llm_busy")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ))
        })
        .collect()
}

/// RTF over the intervals where model work was in flight, and how much audio
/// those intervals decoded.
fn rtf_while_llm(records: &[Record]) -> Option<(f64, f64)> {
    let samples = live_samples(records);
    let (mut busy, mut audio) = (0.0, 0.0);
    for pair in samples.windows(2) {
        if pair[1].3 {
            busy += pair[1].1 - pair[0].1;
            audio += pair[1].2 - pair[0].2;
        }
    }
    (audio >= 5.0).then(|| (busy / audio, audio))
}

fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

pub fn score(records: &[Record]) -> Report {
    let meta = records
        .iter()
        .find(|r| r.kind == RecordType::Meta && r.name == "run-start")
        .map(|r| r.payload.clone())
        .unwrap_or(Value::Null);
    let lines = live_lines(records);
    let first_line_t = lines.first().map(|l| l.t_ms);
    let stop_t = phase_t(records, "stop");
    let finals = final_lines(records);
    let msgs = messages(records);
    let mut criteria = Vec::new();
    let mut stats = BTreeMap::new();

    // Scripted steps: which message each one started.
    let scripts: Vec<&Record> = records
        .iter()
        .filter(|r| r.is_debug(debug::SCRIPT))
        .collect();
    let tag_of: HashMap<String, Option<String>> = scripts
        .iter()
        .filter_map(|r| {
            Some((
                text(&r.payload, "message_id")?.to_string(),
                text(&r.payload, "tag").map(str::to_string),
            ))
        })
        .collect();
    let script_t: HashMap<String, u64> = scripts
        .iter()
        .filter_map(|r| Some((text(&r.payload, "message_id")?.to_string(), r.t_ms)))
        .collect();

    let requests: Vec<CopilotRequest> = msgs
        .last
        .iter()
        .map(|m| {
            let id = text_or(m, "id");
            let stats = m.get("stats").cloned().unwrap_or(Value::Null);
            let deltas = msgs.deltas.get(&id).map(Vec::len).unwrap_or(0);
            // First words as the user sees them: from the ask to the first
            // streamed chunk, when the harness asked; else the helper's own.
            let measured = script_t.get(&id).and_then(|asked| {
                msgs.deltas
                    .get(&id)
                    .and_then(|d| d.first())
                    .map(|first| first.saturating_sub(*asked))
            });
            CopilotRequest {
                tag: tag_of.get(&id).cloned().flatten(),
                kind: text_or(m, "kind"),
                audio_ms: m.get("audioMs").and_then(Value::as_i64).unwrap_or(0),
                status: text_or(m, "status"),
                first_token_ms: measured.or_else(|| uint(&stats, "firstTokenMs")),
                total_ms: uint(&stats, "totalMs"),
                tokens_per_second: num(&stats, "tokensPerSecond"),
                prompt_tokens: uint(&stats, "promptTokens"),
                reused_tokens: uint(&stats, "reusedTokens"),
                deltas,
                error: text(m, "error").map(str::to_string),
                id,
            }
        })
        .collect();
    let asks: Vec<&CopilotRequest> = requests.iter().filter(|r| r.kind != "suggestion").collect();
    let cards: Vec<&CopilotRequest> = requests.iter().filter(|r| r.kind == "suggestion").collect();

    // 1. Idle: no model work before the first line.
    {
        let mut idle = Vec::new();
        if let Some(first) = first_line_t {
            for r in records.iter().filter(|r| r.t_ms < first) {
                if r.is_debug(debug::SIDECAR) {
                    idle.push(format!(
                        "{} sidecar {}",
                        at(r.t_ms),
                        sidecar_what(&r.payload)
                    ));
                }
                if r.is_event("live-timeline-update")
                    && text(&r.payload, "status") == Some("generating")
                {
                    idle.push(format!("{} live summary request", at(r.t_ms)));
                }
            }
            for (id, seen) in &msgs.first_seen {
                if *seen < first && !script_t.contains_key(id) {
                    idle.push(format!("{} copilot message", at(*seen)));
                }
            }
        }
        criteria.push(Criterion {
            id: "idle",
            label: "Idle: no model requests before the first line",
            status: if first_line_t.is_none() {
                Status::NotApplicable
            } else {
                Status::from_bool(idle.is_empty())
            },
            value: format!("{} requests", idle.len()),
            detail: idle,
            private_detail: vec![],
        });
    }

    // 2. First words for the user's own requests.
    {
        let mut values: Vec<u64> = asks.iter().filter_map(|r| r.first_token_ms).collect();
        let detail = asks
            .iter()
            .map(|r| {
                format!(
                    "{} {}{}: first words {} ms, {} total, {:.1} tok/s",
                    at(r.audio_ms.max(0) as u64),
                    r.kind,
                    r.tag
                        .as_ref()
                        .map(|t| format!(" [{t}]"))
                        .unwrap_or_default(),
                    r.first_token_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "–".into()),
                    r.total_ms
                        .map(|v| format!("{v} ms"))
                        .unwrap_or_else(|| "–".into()),
                    r.tokens_per_second.unwrap_or(0.0)
                )
            })
            .collect();
        let max = values.iter().copied().max();
        let missing = asks.iter().filter(|r| r.first_token_ms.is_none()).count();
        criteria.push(Criterion {
            id: "first_words",
            label: "First words ≤ 3 s (local model)",
            status: if asks.is_empty() {
                Status::NotApplicable
            } else {
                Status::from_bool(missing == 0 && max.is_some_and(|m| m <= FIRST_WORDS_LIMIT_MS))
            },
            value: format!(
                "median {} ms, max {} ms{}",
                median(&mut values)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "–".into()),
                max.map(|v| v.to_string()).unwrap_or_else(|| "–".into()),
                if missing > 0 {
                    format!(", {missing} without an answer")
                } else {
                    String::new()
                }
            ),
            detail,
            private_detail: vec![],
        });
    }

    // 3. Streaming.
    {
        let answered: Vec<&CopilotRequest> =
            requests.iter().filter(|r| r.status == "done").collect();
        let streamed = answered.iter().filter(|r| r.deltas >= 2).count();
        criteria.push(Criterion {
            id: "streaming",
            label: "Answers stream (≥ 2 chunks each)",
            status: if answered.is_empty() {
                Status::NotApplicable
            } else {
                Status::from_bool(streamed == answered.len())
            },
            value: format!("{streamed}/{} answers streamed", answered.len()),
            detail: answered
                .iter()
                .filter(|r| r.deltas < 2)
                .map(|r| {
                    format!(
                        "{} {}: {} chunks",
                        at(r.audio_ms.max(0) as u64),
                        r.kind,
                        r.deltas
                    )
                })
                .collect(),
            private_detail: vec![],
        });
    }

    // 4. Prefix reuse on repeat asks.
    {
        let later: Vec<&CopilotRequest> = asks
            .iter()
            .skip(1)
            .copied()
            .filter(|r| r.prompt_tokens.is_some())
            .collect();
        let reused = later
            .iter()
            .filter(|r| r.reused_tokens.unwrap_or(0) > 0)
            .count();
        criteria.push(Criterion {
            id: "reuse",
            label: "Prompt tokens reused on repeat asks",
            status: if later.is_empty() {
                Status::NotApplicable
            } else {
                Status::from_bool(reused == later.len())
            },
            value: format!("{reused}/{} reused", later.len()),
            detail: later
                .iter()
                .map(|r| {
                    format!(
                        "{} {}: {}/{} prompt tokens reused",
                        at(r.audio_ms.max(0) as u64),
                        r.kind,
                        r.reused_tokens.unwrap_or(0),
                        r.prompt_tokens.unwrap_or(0)
                    )
                })
                .collect(),
            private_detail: vec![],
        });
    }

    // 5 + 6. Suggestion cards: latency after the line, and the cooldown.
    {
        let mut latencies = Vec::new();
        let mut detail = Vec::new();
        let mut created: Vec<u64> = Vec::new();
        for card in &cards {
            let Some(seen) = msgs.first_seen.get(&card.id).copied() else {
                continue;
            };
            // The cooldown is a meeting-time rule.
            created.push(msgs.first_seen_audio.get(&card.id).copied().unwrap_or(seen));
            let line = lines.iter().find(|l| {
                l.source == "system"
                    && ((l.start_s * 1000.0).round() as i64 - card.audio_ms).abs() <= 50
            });
            match line {
                Some(line) => {
                    let latency = seen.saturating_sub(line.t_ms);
                    latencies.push(latency);
                    detail.push(format!(
                        "card for the line at {} appeared {} ms after it finalized",
                        at(card.audio_ms.max(0) as u64),
                        latency
                    ));
                }
                None => detail.push(format!(
                    "card at {}: its line was not found",
                    at(card.audio_ms.max(0) as u64)
                )),
            }
        }
        let max = latencies.iter().copied().max();
        criteria.push(Criterion {
            id: "card_latency",
            label: "Suggestion card ≤ 3 s after the line finalizes",
            status: if cards.is_empty() {
                Status::NotApplicable
            } else {
                Status::from_bool(
                    latencies.len() == cards.len()
                        && max.is_some_and(|m| m <= CARD_LATENCY_LIMIT_MS),
                )
            },
            value: format!(
                "{} cards, median {} ms, max {} ms",
                cards.len(),
                median(&mut latencies.clone())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "–".into()),
                max.map(|v| v.to_string()).unwrap_or_else(|| "–".into())
            ),
            detail,
            private_detail: vec![],
        });
        created.sort_unstable();
        let gaps: Vec<u64> = created.windows(2).map(|w| w[1] - w[0]).collect();
        let min_gap = gaps.iter().copied().min();
        criteria.push(Criterion {
            id: "card_cooldown",
            label: "At most one card per 20 s",
            status: if created.len() < 2 {
                Status::NotApplicable
            } else {
                Status::from_bool(min_gap.is_some_and(|g| g >= CARD_COOLDOWN_MS))
            },
            value: format!(
                "min gap {}",
                min_gap
                    .map(|g| format!("{:.1} s", g as f64 / 1000.0))
                    .unwrap_or_else(|| "–".into())
            ),
            detail: vec![],
            private_detail: vec![],
        });
    }

    // 7. Detector activity (for manual false-alarm review).
    {
        let decisions: Vec<&Record> = records
            .iter()
            .filter(|r| r.is_debug(debug::DETECTOR))
            .collect();
        let mut counters: BTreeMap<String, u64> = BTreeMap::new();
        let mut skip_reasons: BTreeMap<String, u64> = BTreeMap::new();
        let mut fired = Vec::new();
        let mut fired_text = Vec::new();
        for d in &decisions {
            let counter = text(&d.payload, "step")
                .or_else(|| text(&d.payload, "counter"))
                .unwrap_or_default()
                .to_string();
            *counters.entry(counter.clone()).or_default() += 1;
            let decision = text_or(&d.payload, "decision");
            if counter == "skipped" {
                let reason = decision
                    .split("skipped: ")
                    .nth(1)
                    .unwrap_or("unknown")
                    .split_whitespace()
                    .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
                    .collect::<Vec<_>>()
                    .join(" ");
                *skip_reasons.entry(reason).or_default() += 1;
            }
            if counter == "confirm_yes" {
                let line_ms = uint(&d.payload, "line_ms").unwrap_or(0);
                fired.push(format!(
                    "fired on the line at {} (wall {})",
                    at(line_ms),
                    at(d.t_ms)
                ));
                if let Some(line) = lines
                    .iter()
                    .find(|l| ((l.start_s * 1000.0).round() as u64).abs_diff(line_ms) <= 50)
                {
                    fired_text.push(format!("[{}] {}", at(line_ms), line.text));
                }
            }
        }
        let others = lines.iter().filter(|l| l.source == "system").count();
        let mut detail = vec![format!(
            "{others} Others lines; decisions: {}",
            counters
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        )];
        if !skip_reasons.is_empty() {
            detail.push(format!(
                "skipped because: {}",
                skip_reasons
                    .iter()
                    .map(|(k, v)| format!("{k} ×{v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        detail.extend(fired);
        stats.insert(
            "detector".into(),
            serde_json::to_value(&counters).unwrap_or_default(),
        );
        criteria.push(Criterion {
            id: "detector",
            label: "Detector fires (review for false alarms)",
            status: Status::Info,
            value: format!(
                "{} fired, {} model no, {} skipped, {} rule rejects",
                counters.get("confirm_yes").copied().unwrap_or(0),
                counters.get("confirm_no").copied().unwrap_or(0),
                counters.get("skipped").copied().unwrap_or(0),
                counters.get("rule_reject").copied().unwrap_or(0)
            ),
            detail,
            private_detail: fired_text,
        });
    }

    // 8. Citations against the final transcript.
    {
        let mut total = 0;
        let mut bad = Vec::new();
        if let Some(finals) = finals.as_ref() {
            for message in &msgs.last {
                let reply = text_or(message, "reply");
                for cited in citations(&reply) {
                    total += 1;
                    let t = cited as f64;
                    let ok = finals.iter().any(|l| {
                        t >= l.start - CITATION_TOLERANCE_S && t <= l.end + CITATION_TOLERANCE_S
                    });
                    if !ok {
                        let nearest = finals
                            .iter()
                            .map(|l| {
                                if t < l.start {
                                    l.start - t
                                } else {
                                    (t - l.end).max(0.0)
                                }
                            })
                            .fold(f64::INFINITY, f64::min);
                        bad.push(format!(
                            "[{}] maps to no line (nearest {:.0} s away)",
                            clock(t),
                            nearest
                        ));
                    }
                }
            }
        }
        criteria.push(Criterion {
            id: "citations",
            label: "[MM:SS] links land on a final-transcript line",
            status: if finals.as_ref().is_none_or(|f| f.is_empty()) || total == 0 {
                Status::NotApplicable
            } else {
                Status::from_bool(bad.is_empty())
            },
            value: format!("{}/{} valid", total - bad.len(), total),
            detail: bad,
            private_detail: vec![],
        });
    }

    // 9. Honesty about things never discussed.
    {
        let probes: Vec<&CopilotRequest> = asks
            .iter()
            .copied()
            .filter(|r| r.tag.as_deref() == Some("honesty"))
            .collect();
        let replies: Vec<(String, bool)> = probes
            .iter()
            .filter_map(|r| {
                let message = msgs.last.iter().find(|m| text(m, "id") == Some(&r.id))?;
                let reply = text_or(message, "reply");
                let ok = honest(&reply);
                Some((reply, ok))
            })
            .collect();
        let ok = replies.iter().filter(|(_, ok)| *ok).count();
        criteria.push(Criterion {
            id: "honesty",
            label: "\"Not discussed\" honesty",
            status: if probes.is_empty() {
                Status::NotApplicable
            } else {
                Status::from_bool(ok == probes.len())
            },
            value: format!("{ok}/{} said it had not come up", probes.len()),
            detail: vec![],
            private_detail: replies.into_iter().map(|(reply, _)| reply).collect(),
        });
    }

    // 10. Live transcription keeps up.
    {
        let windows = rtf_windows(records);
        let worst = windows
            .iter()
            .copied()
            .fold(None::<(u64, f64)>, |acc, w| match acc {
                Some(a) if a.1 >= w.1 => Some(a),
                _ => Some(w),
            });
        let backlog = records
            .iter()
            .filter(|r| r.is_debug(debug::LIVE))
            .filter_map(|r| num(&r.payload, "backlog_s").map(|b| (r.t_ms, b)))
            .fold(None::<(u64, f64)>, |acc, w| match acc {
                Some(a) if a.1 >= w.1 => Some(a),
                _ => Some(w),
            });
        let total = records
            .iter()
            .rev()
            .find(|r| r.is_debug(debug::LIVE))
            .and_then(|r| num(&r.payload, "rtf_total"));
        let overlap = rtf_while_llm(records);
        let over: Vec<String> = windows
            .iter()
            .filter(|(_, rtf)| *rtf >= RTF_LIMIT)
            .map(|(t, rtf)| format!("{} RTF {:.2}", at(*t), rtf))
            .take(20)
            .collect();
        criteria.push(Criterion {
            id: "live_rtf",
            label: "Live transcription RTF < 0.9 (60 s of audio windows)",
            status: match worst {
                None => Status::NotApplicable,
                Some((_, rtf)) => {
                    Status::from_bool(rtf < RTF_LIMIT && overlap.is_none_or(|(o, _)| o < RTF_LIMIT))
                }
            },
            value: format!(
                "max {} (at {}), overall {}, while the model ran {}, max backlog {}",
                worst
                    .map(|w| format!("{:.2}", w.1))
                    .unwrap_or_else(|| "–".into()),
                worst.map(|w| at(w.0)).unwrap_or_else(|| "–".into()),
                total
                    .map(|v| format!("{v:.2}"))
                    .unwrap_or_else(|| "–".into()),
                overlap
                    .map(|(o, a)| format!("{o:.2} over {a:.0} s of audio"))
                    .unwrap_or_else(|| "–".into()),
                backlog
                    .map(|b| format!("{:.1} s at {}", b.1, at(b.0)))
                    .unwrap_or_else(|| "–".into())
            ),
            detail: over,
            private_detail: vec![],
        });
    }

    // 11. Nothing after End meeting.
    {
        let mut after = Vec::new();
        if let Some(stop) = stop_t {
            let limit = stop + AFTER_END_GRACE_MS;
            for r in records.iter().filter(|r| r.t_ms > limit) {
                if r.is_event("copilot-delta") {
                    after.push(format!("{} copilot chunk", at(r.t_ms)));
                }
                if r.is_debug(debug::DETECTOR)
                    && (text(&r.payload, "step") == Some("confirming")
                        || text(&r.payload, "decision").is_some_and(|d| d.contains("confirming")))
                {
                    after.push(format!("{} detector confirm", at(r.t_ms)));
                }
            }
            for (id, seen) in &msgs.first_seen {
                if *seen > limit {
                    after.push(format!("{} new copilot message {id}", at(*seen)));
                }
            }
        }
        criteria.push(Criterion {
            id: "after_end",
            label: "No Copilot work after End meeting",
            status: if stop_t.is_none() {
                Status::NotApplicable
            } else {
                Status::from_bool(after.is_empty())
            },
            value: format!("{} events", after.len()),
            detail: after.into_iter().take(20).collect(),
            private_detail: vec![],
        });
    }

    // 12. Summary determinism.
    {
        let runs: Vec<&Record> = records
            .iter()
            .filter(|r| r.is_debug(debug::SUMMARY))
            .collect();
        let done: Vec<(String, u64)> = runs
            .iter()
            .filter(|r| text(&r.payload, "status") == Some("completed"))
            .map(|r| {
                (
                    text_or(&r.payload, "markdown"),
                    uint(&r.payload, "ms").unwrap_or(0),
                )
            })
            .collect();
        let identical = done.len() >= 2 && done.iter().all(|(m, _)| *m == done[0].0);
        let mut detail: Vec<String> = runs
            .iter()
            .map(|r| {
                format!(
                    "run {}: {} in {:.0} s, {} chars{}",
                    uint(&r.payload, "run").unwrap_or(0),
                    text_or(&r.payload, "status"),
                    uint(&r.payload, "ms").unwrap_or(0) as f64 / 1000.0,
                    text_or(&r.payload, "markdown").chars().count(),
                    text(&r.payload, "error")
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                )
            })
            .collect();
        if done.len() >= 2 && !identical {
            let a: Vec<&str> = done[0].0.lines().collect();
            let b: Vec<&str> = done[1].0.lines().collect();
            let differing =
                a.iter().zip(b.iter()).filter(|(x, y)| x != y).count() + a.len().abs_diff(b.len());
            detail.push(format!("{differing} lines differ"));
        }
        criteria.push(Criterion {
            id: "summary_determinism",
            label: "Summary identical across two runs",
            status: if runs.len() < 2 {
                Status::NotApplicable
            } else {
                Status::from_bool(identical)
            },
            value: format!(
                "{} of {} runs completed, identical: {identical}",
                done.len(),
                runs.len()
            ),
            detail,
            private_detail: vec![],
        });
    }

    // Stats for the report header.
    stats.insert("live_lines".into(), lines.len().into());
    stats.insert(
        "others_lines".into(),
        lines.iter().filter(|l| l.source == "system").count().into(),
    );
    stats.insert("copilot_requests".into(), requests.len().into());
    if let Some(finals) = finals.as_ref() {
        stats.insert("final_lines".into(), finals.len().into());
    }
    let timeline = records
        .iter()
        .rev()
        .find(|r| r.is_event("live-timeline-update"))
        .and_then(|r| {
            r.payload
                .get("entries")
                .and_then(Value::as_array)
                .map(Vec::len)
        });
    stats.insert("timeline_entries".into(), timeline.unwrap_or(0).into());
    let sidecar: Vec<&Record> = records
        .iter()
        .filter(|r| r.is_debug(debug::SIDECAR))
        .collect();
    let mut sidecar_counts: BTreeMap<String, u64> = BTreeMap::new();
    for r in &sidecar {
        *sidecar_counts.entry(sidecar_what(&r.payload)).or_default() += 1;
    }
    stats.insert(
        "sidecar".into(),
        serde_json::to_value(&sidecar_counts).unwrap_or_default(),
    );
    let peak = |key: &str| {
        records
            .iter()
            .filter(|r| r.is_debug(debug::MEMORY))
            .filter_map(|r| num(&r.payload, key))
            .fold(0.0f64, f64::max)
    };
    stats.insert("peak_app_rss_mb".into(), peak("app_rss_mb").round().into());
    stats.insert(
        "peak_helper_rss_mb".into(),
        peak("helper_rss_mb").round().into(),
    );
    if let (Some(start), Some(done)) = (
        phase_t(records, "final-pass-start"),
        phase_t(records, "final-pass-done"),
    ) {
        stats.insert("final_pass_s".into(), ((done - start) / 1000).into());
    }
    let errors: HashSet<String> = requests.iter().filter_map(|r| r.error.clone()).collect();
    stats.insert("copilot_errors".into(), errors.len().into());

    Report {
        schema_version: super::events::SCHEMA_VERSION,
        meta,
        criteria,
        requests,
        stats,
    }
}

/// The human report. Meeting text only when `verbose`.
pub fn to_markdown(report: &Report, verbose: bool) -> String {
    let mut out = String::new();
    let meta = &report.meta;
    out.push_str("# Replay report\n\n");
    out.push_str(&format!(
        "- Recording: `{}` ({:.1} min) at {}× speed\n",
        text(meta, "audio_name").unwrap_or("?"),
        num(meta, "duration_s").unwrap_or(0.0) / 60.0,
        num(meta, "speed").unwrap_or(1.0)
    ));
    out.push_str(&format!(
        "- Live model: {}; Copilot/summary model: {}\n",
        text(meta, "live_engine").unwrap_or("?"),
        text(meta, "summary_model").unwrap_or("?")
    ));
    if let Some(limits) = meta.get("limitations").and_then(Value::as_array) {
        for limit in limits.iter().filter_map(Value::as_str) {
            out.push_str(&format!("- Limitation: {limit}\n"));
        }
    }
    out.push_str(&format!(
        "\n**{} passed, {} failed** of {} criteria.\n\n",
        report.passed(),
        report.failed(),
        report.criteria.len()
    ));
    out.push_str("| Criterion | Result | Value |\n| --- | --- | --- |\n");
    for c in &report.criteria {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            c.label,
            c.status.label(),
            c.value.replace('|', "/")
        ));
    }
    out.push_str("\n## Details\n");
    for c in &report.criteria {
        if c.detail.is_empty() && (!verbose || c.private_detail.is_empty()) {
            continue;
        }
        out.push_str(&format!("\n### {}\n\n", c.label));
        for d in &c.detail {
            out.push_str(&format!("- {d}\n"));
        }
        if verbose {
            for d in &c.private_detail {
                out.push_str(&format!("- > {}\n", d.replace('\n', " ")));
            }
        }
    }
    out.push_str("\n## Stats\n\n");
    for (key, value) in &report.stats {
        out.push_str(&format!("- {key}: {value}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::events::RecordType;
    use serde_json::json;

    fn rec(t_ms: u64, kind: RecordType, name: &str, payload: Value) -> Record {
        Record {
            seq: 0,
            t_ms,
            audio_ms: t_ms,
            kind,
            name: name.into(),
            payload,
        }
    }

    fn ev(t: u64, name: &str, payload: Value) -> Record {
        rec(t, RecordType::Event, name, payload)
    }

    fn dbg(t: u64, name: &str, payload: Value) -> Record {
        rec(t, RecordType::Debug, name, payload)
    }

    fn line(t: u64, source: &str, start: f64, text: &str) -> Record {
        ev(
            t,
            "transcript-update",
            json!({"text": text, "source": source, "is_partial": false,
                   "audio_start_time": start, "audio_end_time": start + 3.0}),
        )
    }

    fn message(
        id: &str,
        kind: &str,
        audio_ms: i64,
        reply: &str,
        status: &str,
        stats: Value,
    ) -> Value {
        json!({"id": id, "kind": kind, "audioMs": audio_ms, "reply": reply,
               "status": status, "stats": stats})
    }

    fn sample() -> Vec<Record> {
        let ask_stats = json!({"firstTokenMs": 900, "totalMs": 4000, "tokensPerSecond": 20.0,
                               "promptTokens": 1200, "reusedTokens": 0});
        let ask2_stats = json!({"firstTokenMs": 700, "totalMs": 3000, "tokensPerSecond": 22.0,
                                "promptTokens": 1300, "reusedTokens": 1100});
        let m1 = message(
            "a1",
            "ask",
            60_000,
            "It has not come up in the meeting so far.",
            "done",
            ask_stats,
        );
        let m2 = message(
            "a2",
            "action",
            120_000,
            "They decided the tenth [00:10].",
            "done",
            ask2_stats,
        );
        let card = message(
            "c1",
            "suggestion",
            100_000,
            "Say yes [01:40].",
            "done",
            json!({}),
        );
        vec![
            rec(
                0,
                RecordType::Meta,
                "run-start",
                json!({"audio_name": "x", "duration_s": 300.0, "speed": 1.0}),
            ),
            dbg(0, "phase", json!({"phase": "live-start"})),
            line(9_000, "mic", 8.0, "Hello everyone"),
            line(12_000, "system", 10.0, "We pick the tenth."),
            dbg(
                30_000,
                "live",
                json!({"backlog_s": 0.2, "busy_s": 5.0, "decoded_audio_s": 30.0, "rtf_total": 0.17}),
            ),
            dbg(
                60_000,
                "script",
                json!({"message_id": "a1", "tag": "honesty"}),
            ),
            ev(60_100, "copilot-state", json!({"messages": [m1.clone()]})),
            ev(60_900, "copilot-delta", json!({"id": "a1", "delta": "It"})),
            ev(
                61_000,
                "copilot-delta",
                json!({"id": "a1", "delta": " has"}),
            ),
            dbg(
                90_000,
                "live",
                json!({"backlog_s": 0.4, "busy_s": 20.0, "decoded_audio_s": 90.0, "rtf_total": 0.22}),
            ),
            line(101_000, "system", 100.0, "Can you send the deck?"),
            dbg(
                101_010,
                "detector",
                json!({"counter": "rule_pass", "decision": "rule: yes (question mark), confirming", "line_ms": 100_000}),
            ),
            dbg(
                102_000,
                "detector",
                json!({"counter": "confirm_yes", "decision": "model: yes", "line_ms": 100_000}),
            ),
            ev(
                102_500,
                "copilot-state",
                json!({"messages": [m1.clone(), card.clone()]}),
            ),
            ev(
                103_000,
                "copilot-delta",
                json!({"id": "c1", "delta": "Say"}),
            ),
            ev(
                103_100,
                "copilot-delta",
                json!({"id": "c1", "delta": " yes"}),
            ),
            dbg(120_000, "script", json!({"message_id": "a2", "tag": null})),
            ev(
                120_000,
                "copilot-state",
                json!({"messages": [m1.clone(), card.clone(), m2.clone()]}),
            ),
            ev(
                120_700,
                "copilot-delta",
                json!({"id": "a2", "delta": "They"}),
            ),
            ev(
                120_800,
                "copilot-delta",
                json!({"id": "a2", "delta": " decided"}),
            ),
            dbg(
                150_000,
                "live",
                json!({"backlog_s": 0.1, "busy_s": 30.0, "decoded_audio_s": 150.0, "rtf_total": 0.2}),
            ),
            dbg(300_000, "phase", json!({"phase": "stop"})),
            dbg(
                400_000,
                "final-transcript",
                json!({"lines": [
                {"start": 8.0, "end": 11.0, "speaker": "You", "text": "Hello"},
                {"start": 99.0, "end": 103.0, "speaker": "Speaker 1", "text": "Can you"}]}),
            ),
            dbg(
                500_000,
                "summary",
                json!({"run": 1, "status": "completed", "markdown": "# A", "ms": 1000}),
            ),
            dbg(
                600_000,
                "summary",
                json!({"run": 2, "status": "completed", "markdown": "# A", "ms": 1100}),
            ),
        ]
    }

    fn status(report: &Report, id: &str) -> Status {
        report.criteria.iter().find(|c| c.id == id).unwrap().status
    }

    #[test]
    fn a_healthy_run_passes_every_criterion() {
        let report = score(&sample());
        for id in [
            "idle",
            "first_words",
            "streaming",
            "reuse",
            "card_latency",
            "citations",
            "honesty",
            "live_rtf",
            "after_end",
            "summary_determinism",
        ] {
            assert_eq!(
                status(&report, id),
                Status::Pass,
                "{id}: {:?}",
                report.criteria
            );
        }
        assert_eq!(status(&report, "card_cooldown"), Status::NotApplicable);
        let first = report
            .criteria
            .iter()
            .find(|c| c.id == "first_words")
            .unwrap();
        // Measured from the ask to the first streamed chunk.
        assert!(first.value.contains("max 900 ms"), "{}", first.value);
        let card = report
            .criteria
            .iter()
            .find(|c| c.id == "card_latency")
            .unwrap();
        assert!(card.value.contains("max 1500 ms"), "{}", card.value);
    }

    #[test]
    fn failures_are_caught() {
        let mut records = sample();
        // A model request while idle, a slow first word, a bad citation, a
        // dishonest answer, a different summary and work after the stop.
        records.insert(2, dbg(1_000, "sidecar", json!({"what": "spawn"})));
        records.push(ev(
            305_000,
            "copilot-delta",
            json!({"id": "a2", "delta": "late"}),
        ));
        let slow = message(
            "a3",
            "ask",
            200_000,
            "They said 5000 dollars [09:59].",
            "done",
            json!({"firstTokenMs": 5000, "promptTokens": 10, "reusedTokens": 0}),
        );
        records.push(dbg(
            200_000,
            "script",
            json!({"message_id": "a3", "tag": "honesty"}),
        ));
        records.push(ev(200_100, "copilot-state", json!({"messages": [slow]})));
        records.push(ev(
            205_000,
            "copilot-delta",
            json!({"id": "a3", "delta": "They"}),
        ));
        records.push(dbg(
            700_000,
            "summary",
            json!({"run": 3, "status": "completed", "markdown": "# B", "ms": 1}),
        ));
        let report = score(&records);
        for id in [
            "idle",
            "first_words",
            "reuse",
            "citations",
            "honesty",
            "after_end",
            "summary_determinism",
        ] {
            assert_eq!(status(&report, id), Status::Fail, "{id}");
        }
    }

    #[test]
    fn rtf_over_the_limit_fails() {
        let records = vec![
            dbg(0, "live", json!({"busy_s": 0.0, "decoded_audio_s": 0.0})),
            dbg(
                30_000,
                "live",
                json!({"busy_s": 28.0, "decoded_audio_s": 30.0}),
            ),
        ];
        let report = score(&records);
        assert_eq!(status(&report, "live_rtf"), Status::Fail);
    }

    #[test]
    fn markdown_keeps_meeting_text_out_unless_verbose() {
        let report = score(&sample());
        let plain = to_markdown(&report, false);
        assert!(plain.contains("| Live transcription RTF"));
        assert!(!plain.contains("Can you send the deck?"));
        assert!(!plain.contains("has not come up"));
        let verbose = to_markdown(&report, true);
        assert!(verbose.contains("Can you send the deck?"));
        assert!(verbose.contains("has not come up"));
    }

    #[test]
    fn honesty_phrases() {
        assert!(honest("That hasn’t come up in the meeting so far."));
        assert!(honest("The budget was not discussed."));
        assert!(!honest("The budget is $5k [12:00]."));
    }
}
