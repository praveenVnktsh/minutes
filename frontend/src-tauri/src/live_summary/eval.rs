//! Developer A/B evaluation of the live notes prompt on replay runs.
//!
//! Reads a replay run's `events.jsonl` (dev/replay-harness), takes its final
//! live transcript segments, and re-runs the live summary windows — the real
//! scheduler, prompt and parser on a simulated clock, with the real built-in
//! model — once with the previous prompt ("old") and once with the current
//! notetaker prompt ("new"). The report is written next to the events, as
//! `live-summary-ab.md`: it quotes meeting content, so it stays on this
//! machine and is never committed.
//!
//! ```text
//! MEETILY_LLAMA_HELPER=target/release/llama-helper \
//! MEETILY_EVAL_REPLAYS="$HOME/Library/Application Support/com.meetily.ai/replays/<run>" \
//!   cargo test -p meetily live_summary_eval -- --ignored --nocapture
//! ```
//!
//! - `MEETILY_EVAL_REPLAYS`: replay folders, separated by `;`.
//! - `MEETILY_EVAL_VARIANTS`: `old,new` (default), or one of them.
//! - `MEETILY_EVAL_UNTIL_MIN`: only the first N minutes (faster iterations).
//! - `MEETILY_EVAL_EXCERPT`: the side-by-side range in minutes, default `6.5-13.5`.
//! - `MEETILY_EVAL_OUT`: report file name, default `live-summary-ab.md`.
//! - `MEETILY_SMOKE_APP_DATA`, `MEETILY_SMOKE_MODEL`: as for the smoke test.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::facts::Figures;
use super::glossary::Glossary;
use super::llm::{generation_options, MAX_OUTPUT_TOKENS};
use super::llm::{GenerateError, Generated, Generator};
use super::notes::MAX_NOTE_WORDS;
use super::notes::{opens_with_speaker, strip_narration, Narration, NoteContext};
use super::parse::{is_repeat, parse_clock_ms, parse_entries, parse_notes, ParsedEntry};
use super::prompt::{self, format_clock, PromptContext};
use super::scheduler::{Segment, Speaker, Window, WindowScheduler};
use super::worker::{
    decide_window_topic, support_text, NotesSoFar, TimelineEntry, TOPIC_STEP_TIMEOUT,
};
use crate::summary::llm_client::{generate_summary, LLMProvider};

/// Earlier notes the parser checks new ones against (as the worker does).
const DEDUPE_HISTORY: usize = 8;
/// The topic step's reply is a few tokens; the worker shares the notes cap.
const TOPIC_STEP_TOKENS: u32 = MAX_OUTPUT_TOKENS;
/// What the previous prompt was capped at.
const LEGACY_MAX_OUTPUT_TOKENS: u32 = 150;
/// A final segment reaches the timeline about this long after its audio ends.
const ARRIVAL_DELAY: Duration = Duration::from_millis(1500);

/// The previous prompt, kept here so the comparison stays reproducible.
mod legacy {
    use super::super::prompt::format_clock;
    use super::super::scheduler::Window;

    pub fn system_prompt() -> &'static str {
        "You keep a live timeline of a meeting while it is being recorded. \
Each request gives you the newest part of the transcript. Write very short timeline entries for it.\n\
\n\
Speaker labels:\n\
- \"You\" is the person recording this meeting (their microphone).\n\
- \"Others\" is everyone else on the call (system audio). It may be several different people.\n\
\n\
Entry kinds:\n\
- \"decision\": something the group agreed or decided.\n\
- \"action\": someone committed to do something (say who, if known).\n\
- \"point\": any other notable topic, fact or question.\n\
\n\
Rules:\n\
- Each \"text\" is at most 15 words, plain language, no filler.\n\
- Only cover the new transcript. Never repeat or rephrase an entry listed as already noted.\n\
- \"start\" and \"end\" are mm:ss times copied from the transcript lines the entry covers.\n\
- \"speaker\" is \"You\", \"Others\" or null.\n\
- If nothing new or meaningful was said, return {\"entries\":[]}.\n\
\n\
Reply with JSON only, no other text:\n\
{\"entries\":[{\"start\":\"mm:ss\",\"end\":\"mm:ss\",\"kind\":\"point\",\"text\":\"...\",\"speaker\":\"Others\"}]}"
    }

    /// (start_ms, kind, text) of earlier entries.
    pub fn user_prompt(window: &Window, previous: &[(i64, String, String)]) -> String {
        let mut prompt = String::new();
        let recent = &previous[previous.len().saturating_sub(3)..];
        if !recent.is_empty() {
            prompt.push_str("Already noted (do not repeat):\n");
            for (start_ms, kind, text) in recent {
                prompt.push_str(&format!(
                    "- [{}] ({kind}) {text}\n",
                    format_clock(*start_ms)
                ));
            }
            prompt.push('\n');
        }
        prompt.push_str(&format!(
            "New transcript ({} to {}):\n",
            format_clock(window.start_ms),
            format_clock(window.end_ms)
        ));
        for segment in &window.segments {
            prompt.push_str(&format!(
                "[{}] {}: {}\n",
                format_clock(segment.start_ms),
                segment.speaker.label(),
                segment.text.trim()
            ));
        }
        let count = if window.max_entries <= 1 {
            "at most 1 entry".to_string()
        } else {
            format!("1 to {} entries", window.max_entries)
        };
        prompt.push_str(&format!(
            "\nReturn {count} for the new transcript as JSON only."
        ));
        prompt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Old,
    New,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

/// One window's request.
struct WindowRun {
    start_ms: i64,
    end_ms: i64,
    words: usize,
    prompt_chars: usize,
    latency: Duration,
    outcome: String,
    kept: usize,
    /// Notes the model wrote that narrate who spoke, before any rewriting.
    raw_narrated: usize,
    /// The topics and kinds the model wrote, before the rules.
    raw_labels: Vec<String>,
    raw_count: usize,
    /// The notes the model wrote, as written, with the window's decided
    /// topic (meeting text: the local report only).
    raw_texts: Vec<String>,
    decided: Option<String>,
}

struct RunResult {
    entries: Vec<TimelineEntry>,
    windows: Vec<WindowRun>,
    /// Model requests made: notes calls and topic steps.
    requests: usize,
}

/// Final live transcript segments of a replay run, on audio time.
pub(super) fn load_segments(events: &Path, until_ms: Option<i64>) -> anyhow::Result<Vec<Segment>> {
    let text = std::fs::read_to_string(events)?;
    let mut segments = Vec::new();
    for line in text.lines() {
        // Cheap filter first: most lines are debug traces.
        if !line.contains("\"transcript-update\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["type"] != "event" || record["name"] != "transcript-update" {
            continue;
        }
        let payload = &record["payload"];
        if payload["is_partial"].as_bool().unwrap_or(false) {
            continue;
        }
        let text = payload["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let seconds = |key: &str| (payload[key].as_f64().unwrap_or(0.0).max(0.0) * 1000.0) as i64;
        let (start_ms, end_ms) = (seconds("audio_start_time"), seconds("audio_end_time"));
        if until_ms.is_some_and(|until| start_ms > until) {
            continue;
        }
        segments.push(Segment {
            start_ms,
            end_ms: end_ms.max(start_ms),
            speaker: Speaker::from_source(payload["source"].as_str().unwrap_or("system")),
            text,
            received_at: Duration::from_millis(end_ms.max(start_ms) as u64) + ARRIVAL_DELAY,
        });
    }
    segments.sort_by_key(|s| s.start_ms);
    Ok(segments)
}

struct Model {
    app_data: PathBuf,
    name: String,
    client: reqwest::Client,
    /// Topic step requests made, for the request rate.
    topic_calls: AtomicUsize,
}

impl Model {
    async fn generate(
        &self,
        system: &str,
        user: &str,
        cap: u32,
    ) -> (Result<String, String>, Duration) {
        let mut options = generation_options();
        options.max_tokens = Some(cap);
        options.output_cap = Some(cap);
        let started = Instant::now();
        let result = generate_summary(
            &self.client,
            &LLMProvider::BuiltInAI,
            &self.name,
            "",
            system,
            user,
            None,
            None,
            &options,
            Some(&self.app_data),
            Some(&CancellationToken::new()),
        )
        .await;
        let elapsed = started.elapsed();
        let result = match result {
            Ok(completion) if completion.truncated => Err("truncated".to_string()),
            Ok(completion) => Ok(completion.content),
            Err(error) => Err(error),
        };
        (result, elapsed)
    }
}

/// The prompt a variant sends for `window`, given the notes so far.
fn build_prompt(
    variant: Variant,
    window: &Window,
    entries: &[TimelineEntry],
    carryover: &[Segment],
    decided: Option<&str>,
) -> (String, String) {
    match variant {
        Variant::Old => {
            let previous: Vec<(i64, String, String)> = entries
                .iter()
                .map(|e| (e.start_ms, e.kind.clone(), e.text.clone()))
                .collect();
            (
                legacy::system_prompt().to_string(),
                legacy::user_prompt(window, &previous),
            )
        }
        Variant::New => {
            let notes = NotesSoFar::of(entries);
            let glossary = notes.glossary.terms();
            let user = prompt::user_prompt(
                window,
                &PromptContext {
                    current_topic: notes.current_topic.as_deref(),
                    previous: &notes.previous,
                    glossary: &glossary,
                    carryover,
                    decided_topic: decided,
                },
            );
            (prompt::system_prompt().to_string(), user)
        }
    }
}

/// The new variant's topic step, as the worker runs it: the decided topic,
/// its latency and its prompt size.
async fn topic_step(
    window: &Window,
    entries: &[TimelineEntry],
    carryover: &[Segment],
    model: &Model,
) -> (Result<Option<String>, String>, Duration, usize) {
    let notes = NotesSoFar::of(entries);
    let glossary = notes.glossary.terms();
    let context = PromptContext {
        current_topic: notes.current_topic.as_deref(),
        previous: &notes.previous,
        glossary: &glossary,
        carryover,
        decided_topic: None,
    };
    let chars = prompt::topic_user_prompt(window, &context)
        .map_or(0, |user| prompt::topic_system_prompt().len() + user.len());
    let asked = model.topic_calls.load(Ordering::SeqCst);
    let started = Instant::now();
    // The worker's own step: skipped while the topic is young, its timeout.
    let decided = decide_window_topic(
        model,
        window,
        &context,
        &notes,
        &CancellationToken::new(),
        TOPIC_STEP_TIMEOUT,
        0,
    )
    .await
    .map_err(|e| e.to_string());
    let was_asked = model.topic_calls.load(Ordering::SeqCst) > asked;
    (
        decided,
        if was_asked {
            started.elapsed()
        } else {
            Duration::ZERO
        },
        if was_asked { chars } else { 0 },
    )
}

/// The eval's model as the worker sees it, so the topic step runs the
/// worker's code.
#[async_trait::async_trait]
impl Generator for Model {
    fn provider(&self) -> &str {
        "builtin-ai"
    }
    fn model(&self) -> &str {
        &self.name
    }
    async fn generate(
        &self,
        system: &str,
        user: &str,
        _cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        if system == prompt::topic_system_prompt() {
            self.topic_calls.fetch_add(1, Ordering::SeqCst);
        }
        let (result, _) = Model::generate(self, system, user, TOPIC_STEP_TOKENS).await;
        match result {
            Ok(text) => Ok(Generated {
                text,
                truncated: false,
            }),
            Err(error) if error == "truncated" => Ok(Generated {
                text: String::new(),
                truncated: true,
            }),
            Err(error) => Err(GenerateError::from_provider_error(error)),
        }
    }
}

/// The reply as a variant's parser reads it.
fn parse_reply(
    variant: Variant,
    raw: &str,
    window: &Window,
    entries: &[TimelineEntry],
    carryover: &[Segment],
    decided: Option<&str>,
) -> Result<Vec<ParsedEntry>, String> {
    let history: Vec<String> = entries
        .iter()
        .rev()
        .take(DEDUPE_HISTORY)
        .map(|e| e.text.clone())
        .collect();
    let result = match variant {
        Variant::Old => parse_entries(
            raw,
            window.start_ms,
            window.end_ms,
            window.max_entries,
            &history,
        ),
        Variant::New => {
            let notes = NotesSoFar::of(entries);
            let figures = Figures::of(&support_text(carryover, &window.segments));
            let context = NoteContext {
                current_topic: notes.current_topic.as_deref(),
                known_topics: &notes.topics,
                glossary: Some(&notes.glossary),
                previous_texts: &history,
                figures: Some(&figures),
                decided_topic: decided,
                drop_fragments: false,
                segments: &window.segments,
            };
            parse_notes(
                raw,
                window.start_ms,
                window.end_ms,
                window.max_entries,
                &context,
            )
        }
    };
    result.map_err(|e| e.to_string())
}

/// Replays the segments through the scheduler on a simulated clock: segments
/// arrive at their audio end time, a request occupies the model for its real
/// latency, and text arriving meanwhile merges into the next window.
async fn run_variant(variant: Variant, segments: &[Segment], model: &Model) -> RunResult {
    let topic_calls_before = model.topic_calls.load(Ordering::SeqCst);
    let mut scheduler = WindowScheduler::new(Duration::ZERO);
    let mut entries: Vec<TimelineEntry> = Vec::new();
    let mut carryover: Vec<Segment> = Vec::new();
    let mut windows = Vec::new();
    let last = segments
        .iter()
        .map(|s| s.received_at)
        .max()
        .unwrap_or_default();
    let mut pending = segments.iter().peekable();
    // A request in flight: its window, when it completes, and its outcome.
    #[allow(clippy::type_complexity)]
    let mut in_flight: Option<(
        Window,
        Duration,
        Result<String, String>,
        Duration,
        usize,
        Option<String>,
    )> = None;
    let cap = match variant {
        Variant::Old => LEGACY_MAX_OUTPUT_TOKENS,
        Variant::New => MAX_OUTPUT_TOKENS,
    };
    let mut now = Duration::ZERO;
    let step = Duration::from_millis(500);
    loop {
        while let Some(segment) = pending.peek() {
            if segment.received_at > now {
                break;
            }
            scheduler.push((*segment).clone());
            pending.next();
        }
        if in_flight
            .as_ref()
            .is_some_and(|(_, done_at, ..)| *done_at <= now)
        {
            let (window, _, result, latency, prompt_chars, decided) = in_flight.take().unwrap();
            scheduler.complete();
            let mut run = WindowRun {
                start_ms: window.start_ms,
                end_ms: window.end_ms,
                words: window.words(),
                prompt_chars,
                latency,
                outcome: String::new(),
                kept: 0,
                raw_narrated: 0,
                raw_labels: Vec::new(),
                raw_count: 0,
                raw_texts: Vec::new(),
                decided: decided.clone(),
            };
            match result {
                Err(error) => {
                    run.outcome = format!("failed: {}", error.chars().take(60).collect::<String>())
                }
                Ok(raw) => {
                    if let Ok(as_written) =
                        parse_entries(&raw, window.start_ms, window.end_ms, 10, &[])
                    {
                        run.raw_count = as_written.len();
                        let figures = Figures::of(&support_text(&carryover, &window.segments));
                        run.raw_texts = as_written
                            .iter()
                            .map(|e| {
                                let missing = figures.unsupported(&e.text);
                                if missing.is_empty() {
                                    e.text.clone()
                                } else {
                                    format!("{} [unsupported {missing:?}]", e.text)
                                }
                            })
                            .collect();
                        run.raw_labels = as_written
                            .iter()
                            .map(|e| {
                                format!("{}/{}", e.topic.as_deref().unwrap_or("-"), e.kind.as_str())
                            })
                            .collect();
                        run.raw_narrated = as_written
                            .iter()
                            .filter(|e| {
                                opens_with_speaker(&e.text)
                                    || strip_narration(&e.text) != Narration::Clean
                            })
                            .count();
                    }
                    match parse_reply(
                        variant,
                        &raw,
                        &window,
                        &entries,
                        &carryover,
                        decided.as_deref(),
                    ) {
                        Ok(parsed) => {
                            run.outcome = "ok".into();
                            run.kept = parsed.len();
                            carryover = window.segments.clone();
                            for entry in parsed {
                                entries.push(TimelineEntry {
                                    id: format!("{}-{}", variant.name(), entries.len()),
                                    start_ms: entry.start_ms,
                                    end_ms: entry.end_ms,
                                    kind: entry.kind.as_str().to_string(),
                                    text: entry.text,
                                    speaker: entry.speaker,
                                    provider: "builtin-ai".into(),
                                    model: model.name.clone(),
                                    created_at: String::new(),
                                    topic: entry.topic,
                                });
                            }
                            entries.sort_by_key(|e| e.start_ms);
                        }
                        Err(error) => {
                            run.outcome = format!(
                                "unreadable: {error}: {:?}",
                                raw.chars().take(300).collect::<String>()
                            )
                        }
                    }
                }
            }
            println!(
                "  [{}] {}..{} {:>4} words {:>5.1}s -> {} ({} kept)",
                variant.name(),
                format_clock(run.start_ms),
                format_clock(run.end_ms),
                run.words,
                run.latency.as_secs_f64(),
                run.outcome,
                run.kept
            );
            windows.push(run);
        }
        let finished = pending.peek().is_none() && now > last;
        if in_flight.is_none() {
            let window = if finished {
                scheduler.flush(now)
            } else {
                scheduler.poll(now, false)
            };
            match window {
                Some(window) => {
                    let (step, step_latency, step_chars) = match variant {
                        Variant::Old => (Ok(None), Duration::ZERO, 0),
                        Variant::New => topic_step(&window, &entries, &carryover, model).await,
                    };
                    let decided = step.clone().ok().flatten();
                    let (system, user) =
                        build_prompt(variant, &window, &entries, &carryover, decided.as_deref());
                    let (result, latency) = match step {
                        // A failed step fails the window, as in the worker.
                        Err(error) => (Err(format!("topic step: {error}")), Duration::ZERO),
                        Ok(_) => model.generate(&system, &user, cap).await,
                    };
                    let latency = latency + step_latency;
                    in_flight = Some((
                        window,
                        now + latency,
                        result,
                        latency,
                        system.len() + user.len() + step_chars,
                        decided,
                    ));
                }
                None if finished => break,
                None => {}
            }
        }
        now += step;
    }
    let requests = windows.len() + model.topic_calls.load(Ordering::SeqCst) - topic_calls_before;
    RunResult {
        entries,
        windows,
        requests,
    }
}

fn word_total(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Aggregate metrics, without meeting text.
fn metrics(
    result: &RunResult,
    duration_ms: i64,
    segments: &[Segment],
) -> BTreeMap<&'static str, String> {
    let entries = &result.entries;
    let n = entries.len();
    let minutes = duration_ms as f64 / 60_000.0;
    let mut out = BTreeMap::new();
    out.insert("a. entries", n.to_string());
    out.insert(
        "b. entries per 10 min",
        format!("{:.1}", n as f64 / (minutes / 10.0).max(0.01)),
    );
    let words: usize = entries.iter().map(|e| word_total(&e.text)).sum();
    out.insert(
        "c. avg words per entry",
        format!("{:.1}", words as f64 / n.max(1) as f64),
    );
    let narrated = entries
        .iter()
        .filter(|e| opens_with_speaker(&e.text) || strip_narration(&e.text) != Narration::Clean)
        .count();
    out.insert(
        "d. % entries narrating speakers (shown)",
        format!("{:.0}%", 100.0 * narrated as f64 / n.max(1) as f64),
    );
    let raw: usize = result.windows.iter().map(|w| w.raw_count).sum();
    let raw_narrated: usize = result.windows.iter().map(|w| w.raw_narrated).sum();
    out.insert(
        "e. % model notes narrating speakers (before rules)",
        format!("{:.0}%", 100.0 * raw_narrated as f64 / raw.max(1) as f64),
    );
    let count = |kind: &str| entries.iter().filter(|e| e.kind == kind).count();
    out.insert("f. actions", count("action").to_string());
    out.insert("g. decisions", count("decision").to_string());
    let mut topics: Vec<&str> = Vec::new();
    for topic in entries.iter().filter_map(|e| e.topic.as_deref()) {
        if !topics.contains(&topic) {
            topics.push(topic);
        }
    }
    out.insert("h. topics", topics.len().to_string());
    let glossary = Glossary::from_texts(entries.iter().map(|e| e.text.as_str()));
    out.insert(
        "i. names spelled more than one way",
        glossary.variant_groups().len().to_string(),
    );
    let latencies: Vec<f64> = result
        .windows
        .iter()
        .map(|w| w.latency.as_secs_f64())
        .collect();
    let mut sorted = latencies.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    out.insert("j. windows", result.windows.len().to_string());
    out.insert(
        "k. latency mean / p50 / max (s)",
        format!(
            "{:.1} / {:.1} / {:.1}",
            latencies.iter().sum::<f64>() / latencies.len().max(1) as f64,
            sorted.get(sorted.len() / 2).copied().unwrap_or(0.0),
            sorted.last().copied().unwrap_or(0.0)
        ),
    );
    let prompt_chars: Vec<usize> = result.windows.iter().map(|w| w.prompt_chars).collect();
    out.insert(
        "l. prompt chars mean / max",
        format!(
            "{} / {}",
            prompt_chars.iter().sum::<usize>() / prompt_chars.len().max(1),
            prompt_chars.iter().max().copied().unwrap_or(0)
        ),
    );
    let failed = result.windows.iter().filter(|w| w.outcome != "ok").count();
    out.insert("m. failed windows", failed.to_string());
    out.insert(
        "m2 requests per hour",
        format!("{:.0}", result.requests as f64 / (minutes / 60.0).max(0.01)),
    );
    out.extend(
        quality(entries, segments)
            .into_iter()
            .map(|(k, v, _)| (k, v)),
    );
    out
}

/// A note's support for the number check in the metrics, the same for
/// every variant (a rendered note has no end): the transcript from 90 s
/// before it to 70 s after (about its window and the context lines before).
fn support_for(entry: &TimelineEntry, segments: &[Segment]) -> String {
    let segments: Vec<Segment> = segments
        .iter()
        .filter(|s| s.start_ms >= entry.start_ms - 90_000 && s.start_ms <= entry.start_ms + 70_000)
        .cloned()
        .collect();
    support_text(&[], &segments)
}

/// Note quality, judged the same way for every variant, with the offending
/// notes (meeting text: printed locally, never in a PR).
pub(super) fn quality(
    entries: &[TimelineEntry],
    segments: &[Segment],
) -> Vec<(&'static str, String, Vec<String>)> {
    let words: Vec<usize> = entries.iter().map(|e| word_total(&e.text)).collect();
    let long: Vec<String> = entries
        .iter()
        .filter(|e| word_total(&e.text) > MAX_NOTE_WORDS)
        .map(render_line)
        .collect();
    let mut repeats = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let earlier = &entries[index.saturating_sub(DEDUPE_HISTORY)..index];
        if let Some(other) = earlier.iter().find(|o| is_repeat(&entry.text, &o.text)) {
            repeats.push(format!("{}  ~  {}", render_line(entry), render_line(other)));
        }
    }
    let unsupported: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            let missing = Figures::of(&support_for(e, segments)).unsupported(&e.text);
            (!missing.is_empty()).then(|| format!("{}  {missing:?}", render_line(e)))
        })
        .collect();
    let actions: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == "action")
        .map(|e| format!("{} (owner {:?})", render_line(e), e.speaker))
        .collect();
    vec![
        (
            "n. max words per entry",
            words.iter().max().copied().unwrap_or(0).to_string(),
            Vec::new(),
        ),
        ("o. entries over 14 words", long.len().to_string(), long),
        (
            "p. repeats of the last 8 notes",
            repeats.len().to_string(),
            repeats,
        ),
        (
            "q. entries with an unsupported number",
            unsupported.len().to_string(),
            unsupported,
        ),
        ("r. actions (listed)", actions.len().to_string(), actions),
    ]
}

/// Notes back from a report's "All notes" block, in either render format.
fn parse_rendered(block: &str) -> Vec<TimelineEntry> {
    let mut entries = Vec::new();
    let mut topic: Option<String> = None;
    for line in block.lines() {
        let line = line.trim_end();
        if let Some(title) = line.strip_prefix("── ").and_then(|l| l.strip_suffix(" ──")) {
            topic = Some(title.to_string());
            continue;
        }
        let mut rest = line;
        let mut kind = "point";
        for (marker, k) in [("ACTION ", "action"), ("DECISION ", "decision")] {
            if let Some(after) = rest.strip_prefix(marker) {
                (rest, kind) = (after, k);
            }
        }
        let Some((clock, text)) = rest.split_once(' ') else {
            continue;
        };
        let Some(start_ms) = parse_clock_ms(clock) else {
            continue;
        };
        let mut text = text.trim_start();
        // The old render puts the marker after the clock.
        for (marker, k) in [("ACTION ", "action"), ("DECISION ", "decision")] {
            if let Some(after) = text.strip_prefix(marker) {
                (text, kind) = (after, k);
            }
        }
        let (mut text, mut speaker) = match text.rsplit_once(" · ") {
            Some((t, s)) if s.split_whitespace().count() <= 3 => {
                (t.to_string(), Some(s.to_string()))
            }
            _ => (text.to_string(), None),
        };
        if let Some(task) = text.strip_prefix("You — ") {
            (text, speaker) = (task.to_string(), Some("You".into()));
        }
        entries.push(TimelineEntry {
            id: entries.len().to_string(),
            start_ms,
            end_ms: start_ms,
            kind: kind.into(),
            text,
            speaker,
            provider: String::new(),
            model: String::new(),
            created_at: String::new(),
            topic: topic.clone(),
        });
    }
    entries
}

/// Rescores the notes of an existing report with the current metrics:
/// `MEETILY_EVAL_RESCORE` is the report path. Prints a table and, locally,
/// the notes behind each count.
#[test]
#[ignore = "developer tool: needs a report from live_summary_eval"]
fn live_summary_rescore() {
    let path =
        PathBuf::from(std::env::var("MEETILY_EVAL_RESCORE").expect("set MEETILY_EVAL_RESCORE"));
    let report = std::fs::read_to_string(&path).expect("the report");
    let segments = load_segments(&path.with_file_name("events.jsonl"), None).expect("events.jsonl");
    let duration_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
    for part in report.split("\n## All notes (").skip(1) {
        let (name, rest) = part.split_once(")").unwrap();
        let block = rest.split("```").nth(1).unwrap_or("");
        let entries = parse_rendered(block);
        let words: usize = entries.iter().map(|e| word_total(&e.text)).sum();
        let mut topics: Vec<&str> = Vec::new();
        for topic in entries.iter().filter_map(|e| e.topic.as_deref()) {
            if !topics.contains(&topic) {
                topics.push(topic);
            }
        }
        println!("\n== {name} ({})", path.display());
        println!("| entries | {} |", entries.len());
        println!(
            "| entries per 10 min | {:.1} |",
            entries.len() as f64 / (duration_ms as f64 / 600_000.0).max(0.01)
        );
        println!(
            "| avg words per entry | {:.1} |",
            words as f64 / entries.len().max(1) as f64
        );
        println!("| topics | {} |", topics.len());
        for (key, value, notes) in quality(&entries, &segments) {
            println!("| {} | {value} |", &key[3..]);
            for note in notes {
                println!("    - {note}");
            }
        }
    }
}

/// One note as the panel shows it.
fn render_line(entry: &TimelineEntry) -> String {
    let marker = match entry.kind.as_str() {
        "action" => "ACTION ",
        "decision" => "DECISION ",
        _ => "",
    };
    let text = match (entry.kind.as_str(), entry.speaker.as_deref()) {
        ("action", Some("You")) => format!("You — {}", entry.text),
        _ => entry.text.clone(),
    };
    format!("{marker}{}  {text}", format_clock(entry.start_ms))
}

/// The notes as the panel groups them: a header when the topic changes.
fn render(entries: &[TimelineEntry], from_ms: i64, to_ms: i64, legacy_speaker: bool) -> String {
    let mut out = String::new();
    let mut topic: Option<&str> = None;
    for entry in entries
        .iter()
        .filter(|e| e.start_ms >= from_ms && e.start_ms <= to_ms)
    {
        if entry.topic.is_some() && entry.topic.as_deref() != topic {
            topic = entry.topic.as_deref();
            let _ = writeln!(out, "── {} ──", topic.unwrap_or(""));
        }
        if legacy_speaker {
            let marker = match entry.kind.as_str() {
                "action" => "ACTION ",
                "decision" => "DECISION ",
                _ => "",
            };
            let speaker = entry
                .speaker
                .as_deref()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{} {marker}{}{speaker}",
                format_clock(entry.start_ms),
                entry.text
            );
        } else {
            let _ = writeln!(out, "{}", render_line(entry));
        }
    }
    out
}

fn env_minutes_range(key: &str, default: (f64, f64)) -> (i64, i64) {
    let (from, to) = std::env::var(key)
        .ok()
        .and_then(|value| {
            let (a, b) = value.split_once('-')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .unwrap_or(default);
    ((from * 60_000.0) as i64, (to * 60_000.0) as i64)
}

#[tokio::test]
#[ignore = "developer tool: needs replay runs, a downloaded built-in model and a release llama-helper"]
async fn live_summary_eval() {
    let replays = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let variants: Vec<Variant> = std::env::var("MEETILY_EVAL_VARIANTS")
        .unwrap_or_else(|_| "old,new".into())
        .split(',')
        .filter_map(|v| match v.trim() {
            "old" => Some(Variant::Old),
            "new" => Some(Variant::New),
            _ => None,
        })
        .collect();
    let until_ms = std::env::var("MEETILY_EVAL_UNTIL_MIN")
        .ok()
        .and_then(|m| m.parse::<f64>().ok())
        .map(|m| (m * 60_000.0) as i64);
    let (excerpt_from, excerpt_to) = env_minutes_range("MEETILY_EVAL_EXCERPT", (6.5, 13.5));
    let out_name =
        std::env::var("MEETILY_EVAL_OUT").unwrap_or_else(|_| "live-summary-ab.md".into());
    let model = Model {
        app_data: std::env::var("MEETILY_SMOKE_APP_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap())
                    .join("Library/Application Support/com.meetily.ai")
            }),
        name: std::env::var("MEETILY_SMOKE_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into()),
        client: reqwest::Client::new(),
        topic_calls: AtomicUsize::new(0),
    };

    for dir in replays.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let segments = load_segments(&dir.join("events.jsonl"), until_ms).expect("events.jsonl");
        let duration_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
        println!(
            "\n== {} ({} segments, {})",
            dir.file_name().unwrap_or_default().to_string_lossy(),
            segments.len(),
            format_clock(duration_ms)
        );

        let mut results = Vec::new();
        for variant in &variants {
            let result = run_variant(*variant, &segments, &model).await;
            results.push((*variant, result));
        }

        let mut report = String::new();
        let _ = writeln!(
            report,
            "# Live summary A/B — {}\n\nLocal only: this file quotes meeting content. Model `{}`, {} segments, {}.\n",
            dir.file_name().unwrap_or_default().to_string_lossy(),
            model.name,
            segments.len(),
            format_clock(duration_ms)
        );
        let _ = writeln!(report, "## Metrics\n");
        let tables: Vec<(Variant, BTreeMap<&str, String>)> = results
            .iter()
            .map(|(v, r)| (*v, metrics(r, duration_ms, &segments)))
            .collect();
        let _ = write!(report, "| metric |");
        for (variant, _) in &tables {
            let _ = write!(report, " {} |", variant.name());
        }
        let _ = write!(report, "\n|---|");
        for _ in &tables {
            let _ = write!(report, "---|");
        }
        report.push('\n');
        if let Some((_, first)) = tables.first() {
            for key in first.keys() {
                let _ = write!(report, "| {} |", &key[3..]);
                for (_, table) in &tables {
                    let _ = write!(report, " {} |", table[key]);
                }
                report.push('\n');
            }
        }
        println!("{report}");

        for (variant, result) in &results {
            let glossary = Glossary::from_texts(result.entries.iter().map(|e| e.text.as_str()));
            let _ = writeln!(
                report,
                "\n## Excerpt {}–{} ({})\n\n```\n{}```",
                format_clock(excerpt_from),
                format_clock(excerpt_to),
                variant.name(),
                render(
                    &result.entries,
                    excerpt_from,
                    excerpt_to,
                    *variant == Variant::Old
                )
            );
            let _ = writeln!(
                report,
                "\nName variants: {:?}\n\n## All notes ({})\n\n```\n{}```",
                glossary.variant_groups(),
                variant.name(),
                render(&result.entries, 0, i64::MAX, *variant == Variant::Old)
            );
            let _ = writeln!(report, "\n### Windows ({})\n", variant.name());
            for window in &result.windows {
                let _ = writeln!(
                    report,
                    "- {}..{} {} words, {} prompt chars, {:.1}s, {} ({} kept, {} written, {} narrated) {:?} topic step: {:?}\n  written: {:?}",
                    format_clock(window.start_ms),
                    format_clock(window.end_ms),
                    window.words,
                    window.prompt_chars,
                    window.latency.as_secs_f64(),
                    window.outcome,
                    window.kept,
                    window.raw_count,
                    window.raw_narrated,
                    window.raw_labels,
                    window.decided,
                    window.raw_texts
                );
            }
        }
        let path = dir.join(&out_name);
        std::fs::write(&path, &report).expect("write the report");
        println!("report: {}", path.display());
    }
    crate::summary::summary_engine::client::shutdown_sidecar_gracefully()
        .await
        .ok();
}
