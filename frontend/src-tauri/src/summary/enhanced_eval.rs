//! Developer A/B of the Enhanced notes on replay runs, with the real
//! built-in models: the old end-of-meeting summary (the summary model over
//! the whole transcript) against the new notes written from the live notes
//! (each closed topic during the meeting, the last one and the title at
//! stop, on the live notes model).
//!
//! The live notes are a saved run of the current live-notes code
//! (`bench/<label>.json` from `live_summary::bench`, default `e2b-goal-r1`).
//! The meeting is replayed on its live transcript: topic sections are written
//! in order as their topics close (model time counted as "during"), then the
//! stop is timed: the last topic, anything missing and the title. The new
//! path runs `MEETILY_EVAL_RUNS` times (default 2) for determinism.
//!
//! The report, `enhanced-live-ab.md` in the replay folder, quotes meeting
//! content: it stays on the machine and is never committed.
//!
//! ```text
//! MEETILY_LLAMA_HELPER=target/release/llama-helper \
//! MEETILY_EVAL_REPLAYS="$HOME/Library/Application Support/com.meetily.ai/replays/<run>;…" \
//!   cargo test -p meetily --lib summary::enhanced_eval -- --ignored --nocapture
//! ```
//!
//! `MEETILY_EVAL_OLD=0` skips the old path; `MEETILY_EVAL_NOTES` names
//! another bench label; `MEETILY_SMOKE_MODEL` is the summary model
//! (default `qwen3.5:4b`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::live_summary::eval::load_segments;
use crate::live_summary::llm::SummaryModelGenerator;
use crate::live_summary::scheduler::Segment;
use crate::summary::budget::ContextBudget;
use crate::summary::live_enhanced::{finish_notes, write_section, NotesInputs, SectionSink};
use crate::summary::live_outline::{
    format_transcript_clock, parse_transcript, parse_transcript_clock, topic_runs, LiveNote,
    TopicRun, WrittenSection,
};
use crate::summary::llm_client::{GenerationOptions, LLMProvider};
use crate::summary::processor::{
    generate_meeting_summary, resolve_summary_system_prompt, ProviderBackend,
};
use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;

static LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[(\d{1,3}:\d{2}(?::\d{2})?)\]").unwrap());

fn transcript_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|s| {
            format!(
                "[{}] [{}] {}",
                format_transcript_clock(s.start_ms),
                s.speaker.label(),
                s.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn load_notes(path: &Path) -> Vec<LiveNote> {
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("bench notes")).unwrap();
    value["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| {
            LiveNote::new(
                e["start_ms"].as_i64().unwrap(),
                e["end_ms"].as_i64().unwrap(),
                e["kind"].as_str().unwrap(),
                e["text"].as_str().unwrap(),
                e["speaker"].as_str(),
                e["topic"].as_str(),
            )
        })
        .collect()
}

struct NoSink;

#[async_trait]
impl SectionSink for NoSink {
    async fn save(&self, _: &WrittenSection, _: i64) {}
}

struct NewRun {
    markdown: String,
    during_s: f64,
    during_calls: usize,
    after_s: f64,
    after_calls: usize,
}

fn section_items(markdown: &str, heading: &str) -> Vec<String> {
    let mut inside = false;
    let mut items = Vec::new();
    for line in markdown.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            inside = title.trim().eq_ignore_ascii_case(heading);
        } else if inside && line.starts_with("- ") {
            items.push(line.to_string());
        }
    }
    items
}

/// Links that land on a transcript line (±2 s), and links inside the time
/// range of the topic section they are in (±60 s).
fn links(markdown: &str, segments: &[Segment], runs: &[TopicRun]) -> (usize, usize, usize) {
    let (mut total, mut on_line, mut in_section) = (0, 0, 0);
    let mut section: Option<&str> = None;
    for line in markdown.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            section = Some(title.trim());
            continue;
        }
        for captures in LINK.captures_iter(line) {
            total += 1;
            let Some(ms) = parse_transcript_clock(&captures[1]) else {
                continue;
            };
            if segments.iter().any(|s| (s.start_ms - ms).abs() <= 2_000) {
                on_line += 1;
            }
            let topic_runs: Vec<&TopicRun> = runs
                .iter()
                .filter(|r| section.is_some_and(|t| t.eq_ignore_ascii_case(&r.title)))
                .collect();
            if topic_runs.is_empty()
                || topic_runs
                    .iter()
                    .any(|r| ms >= r.start_ms - 60_000 && ms <= r.end_ms + 60_000)
            {
                in_section += 1;
            }
        }
    }
    (total, on_line, in_section)
}

async fn new_path(app_data: &Path, notes: &[LiveNote], text: &str) -> NewRun {
    let generator =
        SummaryModelGenerator::builtin_for_eval(app_data.to_path_buf(), LIVE_NOTES_MODEL, true);
    let (lines, _) = parse_transcript(text);
    let end_ms = lines.iter().map(|l| l.ms).max().unwrap_or(0);
    let runs = topic_runs(notes, end_ms);
    let cancel = CancellationToken::new();
    // During the meeting: each closed topic as it closes. A topic closes when
    // the next starts, so the transcript it is written from is its own.
    let mut written = Vec::new();
    println!("== phase: during");
    let started = Instant::now();
    let mut during_calls = 0;
    for run in runs.iter().take(runs.len().saturating_sub(1)) {
        if run.is_small() {
            continue;
        }
        // In the meeting only the transcript heard so far exists.
        let heard: Vec<_> = lines
            .iter()
            .filter(|l| l.ms < run.end_ms)
            .cloned()
            .collect();
        // A failed section is left for the stop, as in the app.
        match write_section(&generator, run, &heard, &[], &cancel).await {
            Ok(section) => written.push(section),
            Err(error) => println!("== section failed: {error}"),
        }
        during_calls += 1;
    }
    let during_s = started.elapsed().as_secs_f64();
    println!("== phase: stop");
    let started = Instant::now();
    let finished = finish_notes(
        &generator,
        NotesInputs {
            notes,
            transcript_text: text,
            user_notes: Vec::new(),
            user_notes_markdown: "",
            stored: written,
            regenerate: false,
        },
        &NoSink,
        &cancel,
    )
    .await
    .expect("finish");
    NewRun {
        markdown: finished.markdown,
        during_s,
        during_calls,
        after_s: started.elapsed().as_secs_f64(),
        after_calls: finished.calls,
    }
}

#[tokio::test]
#[ignore = "developer tool: needs replay runs, downloaded built-in models and a release llama-helper"]
async fn enhanced_live_eval() {
    let replays = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let runs: usize = std::env::var("MEETILY_EVAL_RUNS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(2);
    let with_old = std::env::var("MEETILY_EVAL_OLD").map_or(true, |v| v != "0");
    let label = std::env::var("MEETILY_EVAL_NOTES").unwrap_or_else(|_| "e2b-goal-r1".into());
    let app_data = std::env::var("MEETILY_SMOKE_APP_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap())
                .join("Library/Application Support/com.meetily.ai")
        });
    let summary_model =
        std::env::var("MEETILY_SMOKE_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into());
    let context = crate::summary::summary_engine::get_model_by_name(&summary_model)
        .map_or(32_768, |m| m.context_size as usize);
    let client = reqwest::Client::new();

    for dir in replays.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let segments = load_segments(&dir.join("events.jsonl"), None).expect("events.jsonl");
        let text = transcript_text(&segments);
        let notes = load_notes(&dir.join("bench").join(format!("{label}.json")));
        let duration_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
        let topic_list = topic_runs(&notes, duration_ms);
        let mut report = format!(
            "# Enhanced notes, old vs live — {name}\n\nLocal only: quotes meeting content. {} min, {} live notes ({label}), {} topic runs ({} written by the model).\n\n",
            duration_ms / 60_000,
            notes.len(),
            topic_list.len(),
            topic_list.iter().filter(|r| !r.is_small()).count()
        );

        let mut old: Option<(f64, String)> = None;
        if with_old {
            crate::summary::summary_engine::client::force_shutdown_sidecar()
                .await
                .ok();
            let backend = ProviderBackend {
                client: &client,
                provider: &LLMProvider::BuiltInAI,
                model_name: &summary_model,
                api_key: "",
                ollama_endpoint: None,
                custom_openai_endpoint: None,
                app_data_dir: Some(&app_data),
                cancellation_token: None,
            };
            println!("== phase: old");
            let started = Instant::now();
            let generated = generate_meeting_summary(
                &backend,
                &summary_model,
                &text,
                "",
                &resolve_summary_system_prompt(None, None, ""),
                Some(ContextBudget::new(context)),
                &GenerationOptions::default(),
                None,
                Some("en"),
                None,
            )
            .await
            .expect("old summary");
            let seconds = started.elapsed().as_secs_f64();
            println!("[{name} old] {seconds:.1}s");
            old = Some((seconds, generated.final_markdown));
        }

        crate::summary::summary_engine::client::force_shutdown_sidecar()
            .await
            .ok();
        let mut new_runs = Vec::new();
        for run in 0..runs {
            let result = new_path(&app_data, &notes, &text).await;
            println!(
                "[{name} new #{}] during {:.1}s / {} calls, after stop {:.1}s / {} calls",
                run + 1,
                result.during_s,
                result.during_calls,
                result.after_s,
                result.after_calls
            );
            new_runs.push(result);
        }

        let first = &new_runs[0];
        let identical = new_runs.windows(2).all(|w| w[0].markdown == w[1].markdown);
        let words = |m: &str| m.split_whitespace().count();
        let headings = |m: &str| m.lines().filter(|l| l.starts_with("## ")).count();
        let (total, on_line, in_section) = links(&first.markdown, &segments, &topic_list);
        let _ = writeln!(report, "| | old | new |\n|---|---|---|");
        if let Some((seconds, markdown)) = &old {
            let (o_total, o_on, _) = links(markdown, &segments, &[]);
            let _ = writeln!(
                report,
                "| post-stop time to ready (model) | {seconds:.1} s | {:.1} s |",
                first.after_s
            );
            let _ = writeln!(
                report,
                "| model busy during + after | 0 + {seconds:.1} s | {:.1} + {:.1} s |",
                first.during_s, first.after_s
            );
            let _ = writeln!(
                report,
                "| words | {} | {} |",
                words(markdown),
                words(&first.markdown)
            );
            let _ = writeln!(
                report,
                "| ## sections | {} | {} |",
                headings(markdown),
                headings(&first.markdown)
            );
            let _ = writeln!(
                report,
                "| action items | {} | {} |",
                section_items(markdown, "Action items").len(),
                section_items(&first.markdown, "Action items").len()
            );
            let _ = writeln!(report, "| links (on a line / in section) | {o_total} ({o_on}) | {total} ({on_line} / {in_section}) |");
        } else {
            let _ = writeln!(
                report,
                "| post-stop time to ready (model) | – | {:.1} s |",
                first.after_s
            );
        }
        let _ = writeln!(
            report,
            "| new: calls during / after | | {} / {} |",
            first.during_calls, first.after_calls
        );
        let _ = writeln!(
            report,
            "| new: identical across {runs} runs | | {identical} |"
        );
        for (i, run) in new_runs.iter().enumerate() {
            let _ = writeln!(
                report,
                "| new run {}: during / after | | {:.1} s / {:.1} s |",
                i + 1,
                run.during_s,
                run.after_s
            );
        }
        if let Some((_, markdown)) = &old {
            let _ = writeln!(report, "\n## Old output\n\n````markdown\n{markdown}\n````");
        }
        let _ = writeln!(
            report,
            "\n## New output (run 1)\n\n````markdown\n{}\n````",
            first.markdown
        );
        let path = dir.join("enhanced-live-ab.md");
        std::fs::write(&path, &report).expect("write the report");
        println!("{report}\nreport: {}", path.display());
    }
    crate::summary::summary_engine::client::force_shutdown_sidecar()
        .await
        .ok();
}
