//! Developer benchmark of live notes per (model, runtime config) on replay
//! runs: quality, agreement with a reference run, and cost.
//!
//! Each config drives its own llama-helper process directly (no sidecar
//! manager), so any GGUF, template and runtime option can be tried, and the
//! process's own CPU time, GPU time and peak memory can be read. The window
//! loop is the worker's: the real scheduler on a simulated clock (a request
//! holds the model for its real latency), the real topic step, prompts and
//! parser.
//!
//! ```text
//! MEETILY_LLAMA_HELPER=target/release/llama-helper \
//! MEETILY_BENCH_CONFIGS=/path/configs.json \
//! MEETILY_EVAL_REPLAYS="$HOME/Library/Application Support/com.meetily.ai/replays/<run>;…" \
//!   cargo test -p meetily --lib live_summary::bench::live_notes_bench -- --ignored --nocapture
//! ```
//!
//! `configs.json` is a list of [`BenchConfig`]. A config with
//! `"reference": true` writes `bench/reference.json` in each replay folder;
//! every config is scored against that file when it exists.
//!
//! Outputs: `bench/<label>.json` in the replay folder (notes and per-window
//! costs: meeting text, local only, never committed) and an aggregate line
//! per run, without meeting text, appended to `MEETILY_BENCH_TABLE`
//! (default `live-notes-bench.md` in the current directory).
//!
//! - `MEETILY_EVAL_UNTIL_MIN`: only the first N minutes.
//! - `MEETILY_BENCH_ONLY`: comma-separated labels to run.
//! - `MEETILY_BENCH_PAIRS=1`: print matched/unmatched reference pairs near the
//!   match threshold, to calibrate it by hand (meeting text, local only).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::eval::{load_segments, quality};
use super::facts::Figures;
use super::glossary::Glossary;
use super::llm::{GenerateError, Generated, Generator, MAX_OUTPUT_TOKENS, TEMPERATURE};
use super::notes::{opens_with_speaker, strip_narration, Narration, NoteContext};
use super::parse::{parse_entries, parse_notes, ParsedEntry};
use super::prompt::{self, format_clock, PromptContext};
use super::scheduler::{Segment, Window, WindowScheduler};
use super::worker::{decide_window_topic, folded_topic, support_text, NotesSoFar, TimelineEntry};
use crate::summary::summary_engine::models::{self, SamplingParams};

const DEDUPE_HISTORY: usize = 8;
/// A reference note and a candidate note are the same note when they start
/// within this of each other and their content words agree (`MATCH_DICE`).
const MATCH_WINDOW_MS: i64 = 90_000;
/// Dice overlap of content words at which two notes say the same thing.
/// Calibrated by hand on pairs printed with `MEETILY_BENCH_PAIRS=1`.
const MATCH_DICE: f64 = 0.3;
/// Share of a note's content words found in its transcript (±90 s) for it
/// to count as supported by the transcript.
const SUPPORT_SHARE: f64 = 0.6;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BenchConfig {
    pub label: String,
    pub gguf: PathBuf,
    /// `models::format_prompt` template: `qwen3.5_nonthinking`, `gemma3`, `gemma4`.
    pub template: String,
    /// llama-helper binary; defaults to `MEETILY_LLAMA_HELPER`.
    #[serde(default)]
    pub helper: Option<PathBuf>,
    /// Reuse the stable prefix's state across requests (`cache_prompt`).
    #[serde(default)]
    pub cached: bool,
    /// The model's load context (production sends the catalogue's 32768).
    #[serde(default = "default_context")]
    pub context_size: u32,
    /// n_ctx of the cached context.
    #[serde(default)]
    pub cache_context_size: Option<u32>,
    #[serde(default)]
    pub flash_attn: Option<bool>,
    #[serde(default)]
    pub kv_cache_type: Option<String>,
    #[serde(default)]
    pub n_ubatch: Option<u32>,
    /// Generation threads (production: 2 while recording).
    #[serde(default)]
    pub threads: Option<i32>,
    /// `json` (production) or `compact`.
    #[serde(default = "default_prompt")]
    pub prompt: String,
    /// The separate topic step (production for local models).
    #[serde(default = "default_true")]
    pub topic_step: bool,
    /// Constrain the compact output with its GBNF grammar.
    #[serde(default)]
    pub grammar: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Save this run as the replay's reference.
    #[serde(default)]
    pub reference: bool,
    /// Seed (default the production seed).
    #[serde(default)]
    pub seed: Option<u32>,
    /// Window cadence in seconds (production: `scheduler::CADENCE`).
    #[serde(default)]
    pub cadence_s: Option<u64>,
    /// A system prompt file to try instead of the shipped one.
    #[serde(default)]
    pub system_file: Option<PathBuf>,
    /// A closing instruction to try instead of the user prompt's last line
    /// (`{count}` is replaced by "up to N notes").
    #[serde(default)]
    pub closing: Option<String>,
    /// Notes a normal window may return (production: `ENTRIES_PER_WINDOW`,
    /// plus `COMPACT_EXTRA_NOTES` for the compact prompt); a merged window
    /// gets one more.
    #[serde(default)]
    pub max_entries: Option<usize>,
}

fn default_context() -> u32 {
    32768
}
fn default_prompt() -> String {
    "json".into()
}
fn default_true() -> bool {
    true
}

impl BenchConfig {
    fn sampling(&self) -> SamplingParams {
        match self.template.as_str() {
            "gemma3" => SamplingParams::gemma3_instruct(vec!["<end_of_turn>".into()]),
            "gemma4" => SamplingParams::gemma3_instruct(vec!["<turn|>".into()]),
            _ => SamplingParams::qwen35_summary(vec!["<|im_end|>".into()]),
        }
    }

    fn compact(&self) -> bool {
        self.prompt == "compact"
    }
}

/// One model call's cost, as the helper reported it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallStat {
    pub kind: String,
    pub prompt_tokens: u32,
    pub reused_tokens: u32,
    pub completion_tokens: u32,
    pub first_token_ms: u64,
    pub total_ms: u64,
    pub ok: bool,
}

/// A llama-helper process run under `/usr/bin/time -l`, so its peak memory
/// is printed when it exits.
struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_path: PathBuf,
    next_id: u64,
}

impl Helper {
    async fn spawn(binary: &Path, stderr_path: &Path) -> anyhow::Result<Self> {
        let stderr = std::fs::File::create(stderr_path)?;
        let mut child = tokio::process::Command::new("/usr/bin/time")
            .arg("-l")
            .arg(binary)
            .env("LLAMA_IDLE_TIMEOUT", "100000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr_path: stderr_path.to_path_buf(),
            next_id: 1,
        })
    }

    /// The helper's own pid (the child of `time`).
    fn helper_pid(&self) -> Option<u32> {
        let time_pid = self.child.id()?;
        let out = std::process::Command::new("pgrep")
            .args(["-P", &time_pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    async fn request(&mut self, mut request: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        if request["type"] == "generate" {
            request["id"] = json!(id);
        }
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        while let Some(line) = self.stdout.next_line().await? {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["type"] == "token" {
                continue;
            }
            return Ok(value);
        }
        anyhow::bail!("llama-helper exited")
    }

    /// Shuts the helper down and reads `time -l`'s report: (max RSS bytes,
    /// peak memory footprint bytes).
    async fn finish(mut self) -> (u64, u64) {
        let _ = self.request(json!({"type":"shutdown"})).await;
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await;
        let text = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
        let field = |name: &str| {
            text.lines()
                .find(|l| l.contains(name))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        };
        (
            field("maximum resident set size"),
            field("peak memory footprint"),
        )
    }
}

/// Accumulated GPU time of `pid`'s Metal clients, in seconds (IOKit's
/// per-process `accumulatedGPUTime`, ns).
pub fn gpu_seconds(pid: u32) -> f64 {
    let Ok(out) = std::process::Command::new("ioreg")
        .args(["-r", "-c", "AGXDeviceUserClient", "-l", "-w0"])
        .output()
    else {
        return 0.0;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = format!("\"pid {pid},");
    let mut total: u128 = 0;
    // One block per user client; its properties are sorted, so the usage
    // comes before the creator.
    for block in text.split("+-o ") {
        if !block.contains(&needle) {
            continue;
        }
        for part in block.split("accumulatedGPUTime\"=").skip(1) {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            total += digits.parse::<u128>().unwrap_or(0);
        }
    }
    total as f64 / 1e9
}

/// CPU time (user + system) of `pid`, in seconds.
pub fn cpu_seconds(pid: u32) -> f64 {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
    else {
        return 0.0;
    };
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // [[hh:]mm:]ss.cc
    text.split(':').fold(0.0, |acc, part| {
        acc * 60.0 + part.parse::<f64>().unwrap_or(0.0)
    })
}

/// The model as the worker sees it, driving one helper process.
pub struct BenchModel {
    pub config: BenchConfig,
    helper: Mutex<Option<Helper>>,
    pub calls: Mutex<Vec<CallStat>>,
    topic_calls: AtomicUsize,
}

impl BenchModel {
    pub async fn start(config: BenchConfig, log_dir: &Path) -> anyhow::Result<Self> {
        let binary = config
            .helper
            .clone()
            .or_else(|| {
                std::env::var("MEETILY_LLAMA_HELPER")
                    .ok()
                    .map(PathBuf::from)
            })
            .ok_or_else(|| anyhow::anyhow!("set MEETILY_LLAMA_HELPER or the config's helper"))?;
        std::fs::create_dir_all(log_dir)?;
        let helper = Helper::spawn(&binary, &log_dir.join(format!("{}.log", config.label))).await?;
        Ok(Self {
            config,
            helper: Mutex::new(Some(helper)),
            calls: Mutex::new(Vec::new()),
            topic_calls: AtomicUsize::new(0),
        })
    }

    pub async fn pid(&self) -> Option<u32> {
        self.helper
            .lock()
            .await
            .as_ref()
            .and_then(Helper::helper_pid)
    }

    /// The request the app would send for this prompt.
    pub fn request(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
        grammar: Option<&str>,
    ) -> Value {
        let config = &self.config;
        let sampling = config.sampling().sanitize_for_llama_helper();
        let prompt = models::format_prompt(&config.template, system, user).expect("template");
        let mut request = json!({
            "type": "generate",
            "prompt": prompt,
            "max_tokens": max_tokens,
            "context_size": config.context_size,
            "model_path": config.gguf.to_string_lossy(),
            "temperature": config.temperature.unwrap_or(TEMPERATURE),
            "top_k": sampling.top_k,
            "top_p": sampling.top_p,
            "presence_penalty": sampling.presence_penalty,
            "frequency_penalty": sampling.frequency_penalty,
            "repeat_penalty": sampling.repeat_penalty,
            "penalty_last_n": sampling.penalty_last_n,
            "stop_tokens": sampling.stop_tokens,
            "seed": config.seed.unwrap_or(crate::summary::budget::SUMMARY_SEED),
            "n_threads": config.threads.unwrap_or(
                crate::summary::summary_engine::client::RECORDING_LLM_THREADS),
        });
        if config.cached {
            // The template head and the system prompt are the stable prefix.
            const SENTINEL: &str = "\u{1}USER\u{1}";
            let marked = models::format_prompt(&config.template, system, SENTINEL).unwrap();
            let prefix = marked.find(SENTINEL).unwrap();
            request["cache_prompt"] = json!(true);
            request["cache_prefix_chars"] = json!(prefix);
            request["cache_context_size"] =
                json!(config.cache_context_size.unwrap_or(config.context_size));
        }
        if let Some(flash) = config.flash_attn {
            request["flash_attn"] = json!(flash);
        }
        if let Some(kv) = &config.kv_cache_type {
            request["kv_cache_type"] = json!(kv);
        }
        if let Some(ubatch) = config.n_ubatch {
            request["n_ubatch"] = json!(ubatch);
        }
        if let Some(grammar) = grammar {
            request["grammar"] = json!(grammar);
        }
        request
    }

    pub async fn call(
        &self,
        kind: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        grammar: Option<&str>,
        max_lines: Option<usize>,
    ) -> Result<Generated, GenerateError> {
        let mut request = self.request(system, user, max_tokens, grammar);
        if let Some(max_lines) = max_lines {
            request["max_lines"] = json!(max_lines);
        }
        let started = Instant::now();
        let mut guard = self.helper.lock().await;
        let helper = guard.as_mut().expect("helper running");
        let reply = helper
            .request(request)
            .await
            .map_err(|e| GenerateError::transient(e.to_string()))?;
        let total_ms = started.elapsed().as_millis() as u64;
        let error = reply["error"].as_str().map(str::to_string);
        let stat = CallStat {
            kind: kind.to_string(),
            prompt_tokens: reply["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            reused_tokens: reply["reused_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: reply["completion_tokens"].as_u64().unwrap_or(0) as u32,
            first_token_ms: reply["first_token_ms"].as_u64().unwrap_or(total_ms),
            total_ms,
            ok: error.is_none(),
        };
        self.calls.lock().await.push(stat);
        if let Some(error) = error {
            return Err(GenerateError::transient(error));
        }
        Ok(Generated {
            text: reply["text"].as_str().unwrap_or("").to_string(),
            truncated: reply["finish_reason"] == "length",
        })
    }

    pub async fn finish(&self) -> (u64, u64) {
        match self.helper.lock().await.take() {
            Some(helper) => helper.finish().await,
            None => (0, 0),
        }
    }
}

#[async_trait::async_trait]
impl Generator for BenchModel {
    fn provider(&self) -> &str {
        "builtin-ai"
    }
    fn model(&self) -> &str {
        &self.config.label
    }
    async fn generate(
        &self,
        system: &str,
        user: &str,
        _cancel: &CancellationToken,
    ) -> Result<Generated, GenerateError> {
        // Only the topic step reaches here (the notes call goes through `call`).
        self.topic_calls.fetch_add(1, Ordering::SeqCst);
        self.call("topic", system, user, MAX_OUTPUT_TOKENS, None, None)
            .await
    }
}

/// One window's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchWindow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub words: usize,
    pub latency_ms: u64,
    pub outcome: String,
    pub kept: usize,
    pub raw_count: usize,
    pub raw_narrated: usize,
    pub raw: String,
    pub decided: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchRun {
    pub label: String,
    pub config: BenchConfig,
    pub replay: String,
    pub duration_ms: i64,
    pub entries: Vec<TimelineEntry>,
    pub windows: Vec<BenchWindow>,
    pub calls: Vec<CallStat>,
    pub gpu_s: f64,
    pub cpu_s: f64,
    pub wall_s: f64,
    pub max_rss: u64,
    pub peak_footprint: u64,
}

fn notes_prompts(
    config: &BenchConfig,
    window: &Window,
    context: &PromptContext<'_>,
) -> (String, String) {
    let (system, mut user) = if config.compact() {
        (
            prompt::compact_system_prompt(),
            prompt::compact_user_prompt(window, context),
        )
    } else {
        (
            prompt::system_prompt(),
            prompt::user_prompt(window, context),
        )
    };
    let system = match &config.system_file {
        Some(path) => std::fs::read_to_string(path).expect("system_file"),
        None => system.to_string(),
    };
    if let Some(closing) = &config.closing {
        if let Some(cut) = user.rfind("\n\n") {
            user.truncate(cut + 2);
            let count = format!("up to {} notes", window.max_entries);
            user.push_str(&closing.replace("{count}", &count));
        }
    }
    (system, user)
}

/// Replays the segments through the worker's loop on a simulated clock.
pub async fn run_config(
    model: &BenchModel,
    segments: &[Segment],
) -> (Vec<TimelineEntry>, Vec<BenchWindow>) {
    let config = &model.config;
    let max_tokens = config.max_tokens.unwrap_or(MAX_OUTPUT_TOKENS);
    let grammar = (config.grammar && config.compact()).then(prompt::compact_grammar);
    let mut scheduler = match config.cadence_s {
        Some(seconds) => {
            WindowScheduler::with_cadence(Duration::ZERO, Duration::from_secs(seconds))
        }
        None => WindowScheduler::new(Duration::ZERO),
    };
    let mut entries: Vec<TimelineEntry> = Vec::new();
    let mut carryover: Vec<Segment> = Vec::new();
    let mut windows = Vec::new();
    let last = segments
        .iter()
        .map(|s| s.received_at)
        .max()
        .unwrap_or_default();
    let mut pending = segments.iter().peekable();
    #[allow(clippy::type_complexity)]
    let mut in_flight: Option<(
        Window,
        Duration,
        Result<Generated, GenerateError>,
        Duration,
        Option<String>,
    )> = None;
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
            let (window, _, result, latency, decided) = in_flight.take().unwrap();
            scheduler.complete();
            let mut run = BenchWindow {
                start_ms: window.start_ms,
                end_ms: window.end_ms,
                words: window.words(),
                latency_ms: latency.as_millis() as u64,
                outcome: String::new(),
                kept: 0,
                raw_count: 0,
                raw_narrated: 0,
                raw: String::new(),
                decided: decided.clone(),
            };
            match result {
                Err(error) => {
                    run.outcome = format!(
                        "failed: {}",
                        error.message.chars().take(80).collect::<String>()
                    )
                }
                Ok(generated) if generated.truncated => run.outcome = "failed: truncated".into(),
                Ok(generated) => {
                    let raw = generated.text;
                    run.raw = raw.clone();
                    if let Ok(as_written) =
                        parse_entries(&raw, window.start_ms, window.end_ms, 10, &[])
                    {
                        run.raw_count = as_written.len();
                        run.raw_narrated = as_written
                            .iter()
                            .filter(|e| {
                                opens_with_speaker(&e.text)
                                    || strip_narration(&e.text) != Narration::Clean
                            })
                            .count();
                    }
                    let history: Vec<String> = entries
                        .iter()
                        .rev()
                        .take(DEDUPE_HISTORY)
                        .map(|e| e.text.clone())
                        .collect();
                    let notes = NotesSoFar::of(&entries);
                    // The compact answer's topic line: the folded topic step.
                    let decided = if config.compact() && decided.is_none() {
                        folded_topic(&raw, &notes, window.start_ms)
                    } else {
                        decided
                    };
                    run.decided = decided.clone();
                    let figures = Figures::of(&support_text(&carryover, &window.segments));
                    let context = NoteContext {
                        current_topic: notes.current_topic.as_deref(),
                        known_topics: &notes.topics,
                        glossary: Some(&notes.glossary),
                        previous_texts: &history,
                        figures: Some(&figures),
                        decided_topic: decided.as_deref(),
                        drop_fragments: config.compact(),
                        segments: &window.segments,
                    };
                    match parse_notes(
                        &raw,
                        window.start_ms,
                        window.end_ms,
                        window.max_entries,
                        &context,
                    ) {
                        Ok(parsed) => {
                            run.outcome = "ok".into();
                            run.kept = parsed.len();
                            carryover = window.segments.clone();
                            for entry in parsed {
                                let ParsedEntry {
                                    start_ms,
                                    end_ms,
                                    kind,
                                    text,
                                    speaker,
                                    topic,
                                } = entry;
                                entries.push(TimelineEntry {
                                    id: entries.len().to_string(),
                                    start_ms,
                                    end_ms,
                                    kind: kind.as_str().to_string(),
                                    text,
                                    speaker,
                                    provider: "builtin-ai".into(),
                                    model: config.label.clone(),
                                    created_at: String::new(),
                                    topic,
                                    parent: None,
                                });
                            }
                            entries.sort_by_key(|e| e.start_ms);
                        }
                        Err(error) => run.outcome = format!("unreadable: {error}"),
                    }
                }
            }
            println!(
                "  [{}] {}..{} {:>4} words {:>5.1}s -> {} ({} kept)",
                config.label,
                format_clock(run.start_ms),
                format_clock(run.end_ms),
                run.words,
                run.latency_ms as f64 / 1000.0,
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
                Some(mut window) => {
                    // As the worker: the compact prompt's extra note, unless
                    // the config sets its own count.
                    match config.max_entries {
                        Some(n) => window.max_entries = n + usize::from(window.max_entries > 2),
                        None if config.compact() => {
                            window.max_entries += prompt::COMPACT_EXTRA_NOTES
                        }
                        None => {}
                    }
                    let started = Instant::now();
                    let notes = NotesSoFar::of(&entries);
                    let glossary = notes.glossary.terms();
                    let context = PromptContext {
                        current_topic: notes.current_topic.as_deref(),
                        previous: &notes.previous,
                        glossary: &glossary,
                        carryover: &carryover,
                        decided_topic: None,
                    };
                    let decided = if config.topic_step && !finished {
                        decide_window_topic(
                            model,
                            &window,
                            &context,
                            &notes,
                            &CancellationToken::new(),
                            Duration::from_secs(600),
                            0,
                        )
                        .await
                    } else {
                        Ok(None)
                    };
                    let (result, decided) = match decided {
                        Err(error) => (Err(error), None),
                        Ok(decided) => {
                            let context = PromptContext {
                                decided_topic: decided.as_deref(),
                                ..context
                            };
                            let (system, user) = notes_prompts(config, &window, &context);
                            (
                                model
                                    .call(
                                        "notes",
                                        &system,
                                        &user,
                                        max_tokens,
                                        grammar,
                                        config.compact().then_some(1 + window.max_entries),
                                    )
                                    .await,
                                decided,
                            )
                        }
                    };
                    let latency = started.elapsed();
                    in_flight = Some((window, now + latency, result, latency, decided));
                }
                None if finished => break,
                None => {}
            }
        }
        now += step;
    }
    (entries, windows)
}

// ---------------------------------------------------------------------------
// Agreement with the reference
// ---------------------------------------------------------------------------

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "of", "to", "in", "on", "for", "with", "at", "by",
    "from", "is", "are", "was", "were", "be", "been", "it", "its", "this", "that", "as", "into",
    "about", "their", "they", "them", "has", "have", "had", "not", "no", "via", "over", "than",
    "more", "most", "will", "would", "can", "could", "should", "may", "might", "also", "so", "up",
    "out", "who", "which", "what", "when", "how", "all", "any", "some", "you", "your", "we", "our",
    "i", "he", "she", "his", "her", "do", "does", "did", "being", "very", "just",
];

/// Content words of a note, lowercased and lightly stemmed.
pub fn content_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric() && c != '$' && c != '%') {
        let mut word = raw.to_lowercase();
        if word.is_empty() || STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        for suffix in ["ing", "ed", "es", "s"] {
            if word.len() > suffix.len() + 3 && word.ends_with(suffix) {
                word.truncate(word.len() - suffix.len());
                break;
            }
        }
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

pub fn dice(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let common = a.iter().filter(|w| b.contains(w)).count();
    2.0 * common as f64 / (a.len() + b.len()) as f64
}

fn best_match<'a>(
    note: &TimelineEntry,
    others: &'a [TimelineEntry],
) -> Option<(f64, &'a TimelineEntry)> {
    let tokens = content_tokens(&note.text);
    others
        .iter()
        .filter(|o| (o.start_ms - note.start_ms).abs() <= MATCH_WINDOW_MS)
        .map(|o| (dice(&tokens, &content_tokens(&o.text)), o))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
}

/// The transcript around a note: 90 s before to 70 s after its start.
fn transcript_near(entry: &TimelineEntry, segments: &[Segment]) -> String {
    segments
        .iter()
        .filter(|s| s.start_ms >= entry.start_ms - 90_000 && s.start_ms <= entry.start_ms + 70_000)
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A note whose content words mostly appear in the transcript around it
/// and whose figures are all there.
pub fn supported(entry: &TimelineEntry, segments: &[Segment]) -> bool {
    let near = transcript_near(entry, segments);
    let transcript = content_tokens(&near);
    let tokens = content_tokens(&entry.text);
    if tokens.is_empty() {
        return false;
    }
    let found = tokens
        .iter()
        .filter(|w| {
            transcript.iter().any(|t| {
                t == *w || t.starts_with(w.as_str()) || w.starts_with(t.as_str()) && t.len() >= 4
            })
        })
        .count();
    found as f64 / tokens.len() as f64 >= SUPPORT_SHARE
        && Figures::of(&support_text(&[], &segments_near(entry, segments)))
            .unsupported(&entry.text)
            .is_empty()
}

fn segments_near(entry: &TimelineEntry, segments: &[Segment]) -> Vec<Segment> {
    segments
        .iter()
        .filter(|s| s.start_ms >= entry.start_ms - 90_000 && s.start_ms <= entry.start_ms + 70_000)
        .cloned()
        .collect()
}

/// (coverage, precision, matched-to-reference share of the candidate).
pub fn agreement(
    candidate: &[TimelineEntry],
    reference: &[TimelineEntry],
    segments: &[Segment],
) -> (f64, f64, f64) {
    let covered = reference
        .iter()
        .filter(|r| best_match(r, candidate).is_some_and(|(d, _)| d >= MATCH_DICE))
        .count();
    let matched = candidate
        .iter()
        .filter(|c| best_match(c, reference).is_some_and(|(d, _)| d >= MATCH_DICE))
        .count();
    let precise = candidate
        .iter()
        .filter(|c| {
            best_match(c, reference).is_some_and(|(d, _)| d >= MATCH_DICE) || supported(c, segments)
        })
        .count();
    let ratio = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    (
        ratio(covered, reference.len()),
        ratio(precise, candidate.len()),
        ratio(matched, candidate.len()),
    )
}

/// Words that mark a commitment in the transcript.
const COMMITMENT_CUES: &[&str] = &[
    "i'll",
    "i will",
    "let's",
    "let me",
    "we should",
    "we'll",
    "i can",
    "can you",
    "could you",
    "will you",
    "send",
    "follow up",
    "introduce",
    "reach out",
    "i'm going to",
    "we're going to",
];

/// An action whose transcript around it holds a commitment cue.
pub fn real_action(entry: &TimelineEntry, segments: &[Segment]) -> bool {
    let near = transcript_near(entry, segments).to_lowercase();
    COMMITMENT_CUES.iter().any(|cue| near.contains(cue))
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Aggregate figures for one run, without meeting text.
pub fn summarize(
    run: &BenchRun,
    reference: Option<&[TimelineEntry]>,
    segments: &[Segment],
) -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    let entries = &run.entries;
    let n = entries.len().max(1) as f64;
    let minutes = run.duration_ms as f64 / 60_000.0;
    let hour = 60.0 / minutes.max(0.01);
    out.insert("a notes", entries.len().to_string());
    let narrated = entries
        .iter()
        .filter(|e| opens_with_speaker(&e.text) || strip_narration(&e.text) != Narration::Clean)
        .count();
    out.insert(
        "b narration %",
        format!("{:.0}", 100.0 * narrated as f64 / n),
    );
    let raw: usize = run.windows.iter().map(|w| w.raw_count).sum();
    let raw_narrated: usize = run.windows.iter().map(|w| w.raw_narrated).sum();
    out.insert(
        "b2 raw narration %",
        format!("{:.0}", 100.0 * raw_narrated as f64 / raw.max(1) as f64),
    );
    for (key, value, _) in quality(entries, segments) {
        match key {
            "n. max words per entry" => out.insert("c max words", value),
            "o. entries over 14 words" => out.insert("d >14 words", value),
            "p. repeats of the last 8 notes" => out.insert("e repeats", value),
            "q. entries with an unsupported number" => out.insert("f unsupported numbers", value),
            _ => None,
        };
    }
    let actions: Vec<&TimelineEntry> = entries.iter().filter(|e| e.kind == "action").collect();
    let real = actions.iter().filter(|e| real_action(e, segments)).count();
    let owned = actions.iter().filter(|e| e.speaker.is_some()).count();
    out.insert(
        "g actions/real/owned",
        format!("{}/{}/{}", actions.len(), real, owned),
    );
    out.insert(
        "g2 decisions",
        entries
            .iter()
            .filter(|e| e.kind == "decision")
            .count()
            .to_string(),
    );
    let mut topics: Vec<&str> = Vec::new();
    let mut changes = 0;
    let mut last: Option<&str> = None;
    for topic in entries.iter().filter_map(|e| e.topic.as_deref()) {
        if !topics.contains(&topic) {
            topics.push(topic);
        }
        if last != Some(topic) {
            changes += 1;
            last = Some(topic);
        }
    }
    out.insert(
        "h topics / min per chapter",
        format!("{} / {:.1}", topics.len(), minutes / changes.max(1) as f64),
    );
    let glossary = Glossary::from_texts(entries.iter().map(|e| e.text.as_str()));
    out.insert(
        "i name variants",
        glossary.variant_groups().len().to_string(),
    );
    let failed = run.windows.iter().filter(|w| w.outcome != "ok").count();
    out.insert(
        "j failed windows",
        format!(
            "{}/{} ({:.0}%)",
            failed,
            run.windows.len(),
            100.0 * failed as f64 / run.windows.len().max(1) as f64
        ),
    );
    if let Some(reference) = reference {
        let (coverage, precision, matched) = agreement(entries, reference, segments);
        out.insert("k coverage", format!("{coverage:.2}"));
        out.insert("l precision", format!("{precision:.2}"));
        out.insert("l2 matched share", format!("{matched:.2}"));
    }
    let notes_calls: Vec<&CallStat> = run.calls.iter().filter(|c| c.kind == "notes").collect();
    let all = &run.calls;
    let mean = |f: &dyn Fn(&CallStat) -> f64, calls: &[&CallStat]| {
        calls.iter().map(|c| f(c)).sum::<f64>() / calls.len().max(1) as f64
    };
    let all_refs: Vec<&CallStat> = all.iter().collect();
    out.insert(
        "m calls/h (topic)",
        format!(
            "{:.0} ({:.0})",
            all.len() as f64 * hour,
            all.iter().filter(|c| c.kind == "topic").count() as f64 * hour
        ),
    );
    out.insert(
        "n prompt/reused/gen tok (notes)",
        format!(
            "{:.0}/{:.0}/{:.0}",
            mean(&|c| c.prompt_tokens as f64, &notes_calls),
            mean(&|c| c.reused_tokens as f64, &notes_calls),
            mean(&|c| c.completion_tokens as f64, &notes_calls)
        ),
    );
    out.insert(
        "o first-token/gen ms (notes)",
        format!(
            "{:.0}/{:.0}",
            mean(&|c| c.first_token_ms as f64, &notes_calls),
            mean(
                &|c| c.total_ms.saturating_sub(c.first_token_ms) as f64,
                &notes_calls
            )
        ),
    );
    let mut latencies: Vec<u64> = run.windows.iter().map(|w| w.latency_ms).collect();
    latencies.sort_unstable();
    out.insert(
        "p window s mean/p50/max",
        format!(
            "{:.1}/{:.1}/{:.1}",
            latencies.iter().sum::<u64>() as f64 / latencies.len().max(1) as f64 / 1000.0,
            latencies.get(latencies.len() / 2).copied().unwrap_or(0) as f64 / 1000.0,
            latencies.last().copied().unwrap_or(0) as f64 / 1000.0
        ),
    );
    let busy: f64 = all_refs.iter().map(|c| c.total_ms as f64 / 1000.0).sum();
    out.insert(
        "q busy s/h (% of hour)",
        format!("{:.0} ({:.1}%)", busy * hour, busy * hour / 36.0),
    );
    out.insert(
        "r GPU s/h (% of hour)",
        format!("{:.0} ({:.1}%)", run.gpu_s * hour, run.gpu_s * hour / 36.0),
    );
    out.insert(
        "s CPU s/h (% of a core-hour)",
        format!("{:.0} ({:.1}%)", run.cpu_s * hour, run.cpu_s * hour / 36.0),
    );
    out.insert(
        "t peak RSS / footprint MB",
        format!(
            "{:.0} / {:.0}",
            run.max_rss as f64 / 1048576.0,
            run.peak_footprint as f64 / 1048576.0
        ),
    );
    out
}

fn load_configs() -> Vec<BenchConfig> {
    let path = std::env::var("MEETILY_BENCH_CONFIGS").expect("set MEETILY_BENCH_CONFIGS");
    let configs: Vec<BenchConfig> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("configs file"))
            .expect("configs json");
    let only: Option<Vec<String>> = std::env::var("MEETILY_BENCH_ONLY")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect());
    configs
        .into_iter()
        .filter(|c| only.as_ref().is_none_or(|only| only.contains(&c.label)))
        .collect()
}

fn print_pairs(candidate: &[TimelineEntry], reference: &[TimelineEntry]) {
    println!("\n-- calibration pairs (local only)");
    for r in reference {
        if let Some((d, c)) = best_match(r, candidate) {
            if (0.15..0.5).contains(&d) {
                println!("{d:.2} | {} | {}", r.text, c.text);
            }
        }
    }
}

#[tokio::test]
#[ignore = "developer tool: needs replay runs, GGUFs and a release llama-helper"]
async fn live_notes_bench() {
    let replays = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let until_ms = std::env::var("MEETILY_EVAL_UNTIL_MIN")
        .ok()
        .and_then(|m| m.parse::<f64>().ok())
        .map(|m| (m * 60_000.0) as i64);
    let table_path = PathBuf::from(
        std::env::var("MEETILY_BENCH_TABLE").unwrap_or_else(|_| "live-notes-bench.md".into()),
    );
    let pairs = std::env::var("MEETILY_BENCH_PAIRS").is_ok();
    for config in load_configs() {
        for dir in replays.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            let replay = dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let segments =
                load_segments(&dir.join("events.jsonl"), until_ms).expect("events.jsonl");
            let duration_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
            let bench_dir = dir.join("bench");
            println!(
                "\n== {} on {} ({})",
                config.label,
                replay,
                format_clock(duration_ms)
            );
            let model = BenchModel::start(config.clone(), &bench_dir)
                .await
                .expect("helper starts");
            // Warm up: load the model and build the context, as an app that
            // has already served a window would have.
            let window = Window {
                start_ms: 0,
                end_ms: 1000,
                segments: segments.iter().take(3).cloned().collect(),
                max_entries: 1,
                dropped_segments: 0,
            };
            let context = PromptContext::default();
            let (system, user) = notes_prompts(&config, &window, &context);
            let load_started = Instant::now();
            model
                .call("warmup", &system, &user, 1, None, None)
                .await
                .expect("warmup");
            let load_s = load_started.elapsed().as_secs_f64();
            model.calls.lock().await.clear();
            let pid = model.pid().await.expect("helper pid");
            let (gpu0, cpu0) = (gpu_seconds(pid), cpu_seconds(pid));
            let started = Instant::now();
            let (entries, windows) = run_config(&model, &segments).await;
            let wall_s = started.elapsed().as_secs_f64();
            let (gpu1, cpu1) = (gpu_seconds(pid), cpu_seconds(pid));
            let calls = model.calls.lock().await.clone();
            let (max_rss, peak_footprint) = model.finish().await;
            let run = BenchRun {
                label: config.label.clone(),
                config: config.clone(),
                replay: replay.clone(),
                duration_ms,
                entries,
                windows,
                calls,
                gpu_s: gpu1 - gpu0,
                cpu_s: cpu1 - cpu0,
                wall_s,
                max_rss,
                peak_footprint,
            };
            std::fs::create_dir_all(&bench_dir).unwrap();
            std::fs::write(
                bench_dir.join(format!("{}.json", config.label)),
                serde_json::to_string_pretty(&run).unwrap(),
            )
            .unwrap();
            if config.reference {
                std::fs::write(
                    bench_dir.join("reference.json"),
                    serde_json::to_string_pretty(&run.entries).unwrap(),
                )
                .unwrap();
            }
            let reference: Option<Vec<TimelineEntry>> =
                std::fs::read_to_string(bench_dir.join("reference.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok());
            if pairs {
                if let Some(reference) = &reference {
                    print_pairs(&run.entries, reference);
                }
            }
            let summary = summarize(&run, reference.as_deref(), &segments);
            let mut line = format!(
                "| {} | {} | {:.1} |",
                config.label,
                replay.split('-').last().unwrap_or(""),
                load_s
            );
            let mut header = String::from("| config | replay | load s |");
            for (key, value) in &summary {
                let _ = write!(line, " {value} |");
                let _ = write!(
                    header,
                    " {} |",
                    key.split_once(' ').map_or(*key, |(_, k)| k)
                );
            }
            println!("{header}\n{line}");
            let mut table = std::fs::read_to_string(&table_path).unwrap_or_default();
            if !table.contains(&header) {
                table.push_str(&format!(
                    "\n{header}\n|{}\n",
                    "---|".repeat(summary.len() + 3)
                ));
            }
            table.push_str(&line);
            table.push('\n');
            std::fs::write(&table_path, table).unwrap();
        }
    }
}

/// Rescores saved runs (`bench/<label>.json`) against the current reference
/// and metrics, without running a model: `MEETILY_BENCH_RESCORE` lists the
/// labels; prints the table.
#[test]
#[ignore = "developer tool: needs saved bench runs"]
fn live_notes_rescore() {
    let replays = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let labels = std::env::var("MEETILY_BENCH_RESCORE").expect("set MEETILY_BENCH_RESCORE");
    let pairs = std::env::var("MEETILY_BENCH_PAIRS").is_ok();
    for dir in replays.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let segments = load_segments(&dir.join("events.jsonl"), None).expect("events.jsonl");
        let reference: Option<Vec<TimelineEntry>> =
            std::fs::read_to_string(dir.join("bench/reference.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
        for label in labels.split(',') {
            let Ok(text) = std::fs::read_to_string(dir.join(format!("bench/{label}.json"))) else {
                continue;
            };
            let run: BenchRun = serde_json::from_str(&text).unwrap();
            let summary = summarize(&run, reference.as_deref(), &segments);
            println!(
                "\n== {label} on {}",
                dir.file_name().unwrap_or_default().to_string_lossy()
            );
            for (key, value) in summary {
                println!("| {key} | {value} |");
            }
            if pairs {
                if let Some(reference) = &reference {
                    print_pairs(&run.entries, reference);
                }
            }
        }
    }
}

/// Agreement between saved runs, to separate a config's quality from run to
/// run noise: each candidate label (`MEETILY_BENCH_CANDS`) is scored against
/// each reference label (`MEETILY_BENCH_REFS`) on every replay, and the mean,
/// min and max coverage and precision are printed. A label in both lists is
/// not scored against itself. Aggregates only.
#[test]
#[ignore = "developer tool: needs saved bench runs"]
fn live_notes_noise() {
    let replays = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let refs = std::env::var("MEETILY_BENCH_REFS").expect("set MEETILY_BENCH_REFS");
    let cands = std::env::var("MEETILY_BENCH_CANDS").expect("set MEETILY_BENCH_CANDS");
    let load = |dir: &Path, label: &str| -> Option<BenchRun> {
        let text = std::fs::read_to_string(dir.join(format!("bench/{label}.json"))).ok()?;
        serde_json::from_str(&text).ok()
    };
    let stats = |values: &[f64]| {
        let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        format!("{mean:.2} [{min:.2}–{max:.2}] n={}", values.len())
    };
    for dir in replays.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let segments = load_segments(&dir.join("events.jsonl"), None).expect("events.jsonl");
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        println!("\n== {name}");
        let (mut all_cov, mut all_prec) = (Vec::new(), Vec::new());
        for cand in cands.split(',').map(str::trim) {
            let Some(c) = load(&dir, cand) else { continue };
            let (mut cov, mut prec) = (Vec::new(), Vec::new());
            for reference in refs.split(',').map(str::trim).filter(|r| *r != cand) {
                let Some(r) = load(&dir, reference) else {
                    continue;
                };
                let (coverage, precision, _) = agreement(&c.entries, &r.entries, &segments);
                println!(
                    "| {cand} vs {reference} | coverage {coverage:.2} | precision {precision:.2} |"
                );
                cov.push(coverage);
                prec.push(precision);
            }
            println!(
                "| {cand} | coverage {} | precision {} |",
                stats(&cov),
                stats(&prec)
            );
            all_cov.extend(cov);
            all_prec.extend(prec);
        }
        println!(
            "| ALL | coverage {} | precision {} |",
            stats(&all_cov),
            stats(&all_prec)
        );
    }
}

/// 16 kHz mono PCM16 WAV samples.
fn read_wav_16k(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    let mut offset = 12;
    loop {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            return bytes[offset + 8..offset + 8 + size]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
        }
        offset += 8 + size;
    }
}

/// Live transcription's real-time factor alone and while a config's live
/// notes requests run back to back (the worst case: in a meeting they take
/// the "busy % of hour" share of the time). Needs
/// `NEMOTRON_STREAMING_MODEL_DIR` (with `test_wavs/0.wav`), a replay for
/// realistic prompts, and the configs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "developer tool: needs the Nemotron model, GGUFs and a release llama-helper"]
async fn live_notes_rtf() {
    use crate::audio::live_streaming::transcriber::{LiveEvent, Source, StreamingTranscriber};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let dir = PathBuf::from(
        std::env::var("NEMOTRON_STREAMING_MODEL_DIR").expect("set NEMOTRON_STREAMING_MODEL_DIR"),
    );
    let replay = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let replay = PathBuf::from(replay.split(';').next().unwrap().trim());
    let seconds: usize = std::env::var("MEETILY_RTF_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);
    let engine = Arc::new(crate::audio::live_streaming::engine::create(&dir).expect("model loads"));
    let speech = read_wav_16k(&dir.join("test_wavs").join("0.wav"));
    let sr = 48_000usize;
    let mut system = Vec::with_capacity(seconds * sr);
    while system.len() < seconds * sr {
        system.extend(speech.iter().flat_map(|&s| [s, s, s]));
        system.extend(std::iter::repeat_n(0.0, sr));
    }
    system.truncate(seconds * sr);
    let mut mic = vec![0.0; 3 * sr];
    mic.extend_from_slice(&system);
    mic.truncate(seconds * sr);
    let audio = Arc::new((system, mic));
    let run = move |engine: Arc<_>, audio: Arc<(Vec<f32>, Vec<f32>)>| -> f64 {
        let mut transcriber = StreamingTranscriber::new(engine, sr as u32);
        let mut events: Vec<LiveEvent> = Vec::new();
        let window = sr / 20;
        let started = Instant::now();
        for (i, (sys, mic)) in audio
            .0
            .chunks(window)
            .zip(audio.1.chunks(window))
            .enumerate()
        {
            let start = (i * window) as f64 / sr as f64;
            transcriber.push(Source::System, sys, start, &mut events);
            transcriber.push(Source::Microphone, mic, start, &mut events);
            if i % 11 == 10 {
                transcriber.decode(&mut events);
            }
        }
        transcriber.finish(&mut events);
        started.elapsed().as_secs_f64() / seconds as f64
    };
    let alone = {
        let (engine, audio) = (engine.clone(), audio.clone());
        tokio::task::spawn_blocking(move || run(engine, audio))
            .await
            .unwrap()
    };
    println!("live alone: RTF {alone:.3}");

    // Real window prompts: the replay's transcript in 60 s windows.
    let segments = load_segments(&replay.join("events.jsonl"), None).expect("events.jsonl");
    let mut windows: Vec<Window> = Vec::new();
    for chunk in segments.chunk_by(|a, b| a.start_ms / 60_000 == b.start_ms / 60_000) {
        windows.push(Window {
            start_ms: chunk[0].start_ms,
            end_ms: chunk.last().unwrap().end_ms,
            segments: chunk.to_vec(),
            max_entries: 2,
            dropped_segments: 0,
        });
    }
    for config in load_configs() {
        let model = Arc::new(
            BenchModel::start(config.clone(), &replay.join("bench"))
                .await
                .expect("helper"),
        );
        let grammar = (config.grammar && config.compact()).then(prompt::compact_grammar);
        let max_tokens = config.max_tokens.unwrap_or(MAX_OUTPUT_TOKENS);
        let context = PromptContext::default();
        let (system, user) = notes_prompts(&config, &windows[0], &context);
        model
            .call("warmup", &system, &user, 1, None, None)
            .await
            .expect("warmup");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = {
            let (model, stop, windows, config) =
                (model.clone(), stop.clone(), windows.clone(), config.clone());
            tokio::spawn(async move {
                let mut i = 0;
                while !stop.load(Ordering::SeqCst) {
                    let window = &windows[i % windows.len()];
                    let (system, user) = notes_prompts(&config, window, &PromptContext::default());
                    let _ = model
                        .call("notes", &system, &user, max_tokens, grammar, None)
                        .await;
                    i += 1;
                }
                i
            })
        };
        let contended = {
            let (engine, audio) = (engine.clone(), audio.clone());
            tokio::task::spawn_blocking(move || run(engine, audio))
                .await
                .unwrap()
        };
        stop.store(true, Ordering::SeqCst);
        let count = requests.await.unwrap();
        model.finish().await;
        println!(
            "| {} | RTF alone {alone:.3} | back to back {contended:.3} ({:.2}x) | {count} requests |",
            config.label,
            contended / alone
        );
    }
}
