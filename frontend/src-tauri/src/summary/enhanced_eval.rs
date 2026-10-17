//! Developer A/B evaluation of enhanced notes: the transcript path ("old")
//! against the live-notes path ("new"), on replay runs, with the real
//! built-in model.
//!
//! For each run it first makes live notes with the current notetaker
//! (`live_summary::eval`'s "new" variant over the run's live transcript) and
//! caches them as `live-notes-new.json`, then generates the enhanced notes
//! each way `MEETILY_EVAL_RUNS` times (default 2, for determinism), with a
//! fresh llama-helper per run. The report, `enhanced-ab.md`, quotes meeting
//! content: it stays next to the run and is never committed.
//!
//! ```text
//! MEETILY_LLAMA_HELPER=target/release/llama-helper \
//! MEETILY_EVAL_REPLAYS="$HOME/Library/Application Support/com.meetily.ai/replays/<run>" \
//!   cargo test -p meetily enhanced_notes_eval -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

use crate::live_summary::eval::{load_segments, run_variant, Model, Variant};
use crate::live_summary::scheduler::Segment;
use crate::summary::budget::ContextBudget;
use crate::summary::live_outline::{
    build_outline, format_transcript_clock, parse_transcript_clock, LiveNote,
};
use crate::summary::llm_client::{GenerationOptions, LLMProvider};
use crate::summary::processor::{
    generate_meeting_summary, resolve_summary_system_prompt, ProviderBackend,
};

static LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[(\d{1,3}:\d{2}(?::\d{2})?)\]").unwrap());
/// A link resolves when a transcript line starts this close to it.
const RESOLVE_MS: i64 = 30_000;

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

fn load_cached_notes(path: &PathBuf) -> Option<Vec<LiveNote>> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    value
        .as_array()?
        .iter()
        .map(|e| {
            Some(LiveNote::new(
                e["start_ms"].as_i64()?,
                e["end_ms"].as_i64()?,
                e["kind"].as_str()?,
                e["text"].as_str()?,
                e["speaker"].as_str(),
                e["topic"].as_str(),
            ))
        })
        .collect()
}

struct Output {
    seconds: f64,
    markdown: String,
    calls: i64,
}

fn section_items(markdown: &str, heading: &str) -> usize {
    let mut inside = false;
    let mut count = 0;
    for line in markdown.lines() {
        if line.starts_with("## ") {
            inside = line
                .trim_start_matches("## ")
                .trim()
                .eq_ignore_ascii_case(heading);
        } else if inside && line.trim_start().starts_with("- ") && !line.starts_with("  ") {
            count += 1;
        }
    }
    count
}

fn links(markdown: &str, segments: &[Segment]) -> (usize, usize) {
    let mut total = 0;
    let mut resolved = 0;
    for captures in LINK.captures_iter(markdown) {
        total += 1;
        if let Some(ms) = parse_transcript_clock(&captures[1]) {
            if segments
                .iter()
                .any(|s| (s.start_ms - ms).abs() <= RESOLVE_MS)
            {
                resolved += 1;
            }
        }
    }
    (total, resolved)
}

#[tokio::test]
#[ignore = "developer tool: needs replay runs, a downloaded built-in model and a release llama-helper"]
async fn enhanced_notes_eval() {
    let replays = std::env::var("MEETILY_EVAL_REPLAYS").expect("set MEETILY_EVAL_REPLAYS");
    let runs: usize = std::env::var("MEETILY_EVAL_RUNS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(2);
    let app_data = std::env::var("MEETILY_SMOKE_APP_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap())
                .join("Library/Application Support/com.meetily.ai")
        });
    let model_name = std::env::var("MEETILY_SMOKE_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into());
    let context = crate::summary::summary_engine::get_model_by_name(&model_name)
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
        let duration_ms = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
        println!(
            "\n== {name} ({} segments, {})",
            segments.len(),
            format_transcript_clock(duration_ms)
        );

        let notes_path = dir.join("live-notes-new.json");
        let notes = match load_cached_notes(&notes_path) {
            Some(notes) => notes,
            None => {
                let model = Model {
                    app_data: app_data.clone(),
                    name: model_name.clone(),
                    client: client.clone(),
                    topic_calls: AtomicUsize::new(0),
                };
                let started = Instant::now();
                let result = run_variant(Variant::New, &segments, &model).await;
                println!(
                    "live notes: {} in {:.0}s",
                    result.entries.len(),
                    started.elapsed().as_secs_f64()
                );
                let rows: Vec<Value> = result
                    .entries
                    .iter()
                    .map(|e| {
                        json!({"start_ms": e.start_ms, "end_ms": e.end_ms, "kind": e.kind,
                               "text": e.text, "speaker": e.speaker, "topic": e.topic})
                    })
                    .collect();
                std::fs::write(&notes_path, serde_json::to_string_pretty(&rows).unwrap())
                    .expect("write live notes");
                load_cached_notes(&notes_path).expect("re-read live notes")
            }
        };
        let text = transcript_text(&segments);
        let outline = build_outline(&notes, &text, "");
        let mut report = format!(
            "# Enhanced notes A/B — {name}\n\nLocal only: this file quotes meeting content. Model `{model_name}`, {} segments, {}, {} live notes (new notetaker).\n\n",
            segments.len(),
            format_transcript_clock(duration_ms),
            notes.len()
        );
        match &outline {
            Ok(o) => {
                let _ = writeln!(
                    report,
                    "Outline: {} sections: {}\n",
                    o.sections.len(),
                    o.sections
                        .iter()
                        .map(|s| format!(
                            "{} ({} notes, {} user)",
                            s.title,
                            s.notes.len(),
                            s.user_notes.len()
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
            Err(reason) => {
                let _ = writeln!(report, "Outline fell back: {reason}\n");
            }
        }

        let budget = ContextBudget::new(context);
        let mut results: Vec<(&str, Vec<Output>)> = Vec::new();
        for variant in ["old", "new"] {
            let outline = match (variant, &outline) {
                ("new", Ok(o)) => Some(o),
                ("new", Err(_)) => continue,
                _ => None,
            };
            let mut outputs = Vec::new();
            for run in 0..runs {
                crate::summary::summary_engine::force_shutdown_sidecar()
                    .await
                    .ok();
                let backend = ProviderBackend {
                    client: &client,
                    provider: &LLMProvider::BuiltInAI,
                    model_name: &model_name,
                    api_key: "",
                    ollama_endpoint: None,
                    custom_openai_endpoint: None,
                    app_data_dir: Some(&app_data),
                    cancellation_token: None,
                };
                let started = Instant::now();
                let generated = generate_meeting_summary(
                    &backend,
                    &model_name,
                    &text,
                    "",
                    &resolve_summary_system_prompt(None, None, ""),
                    Some(budget),
                    &GenerationOptions::default(),
                    None,
                    Some("en"),
                    None,
                    outline,
                )
                .await
                .expect("summary");
                let seconds = started.elapsed().as_secs_f64();
                println!(
                    "[{variant} #{}] {seconds:.0}s, {} chars",
                    run + 1,
                    generated.final_markdown.len()
                );
                outputs.push(Output {
                    seconds,
                    markdown: generated.final_markdown,
                    calls: generated.successful_chunk_count,
                });
            }
            results.push((variant, outputs));
        }

        report.push_str("| metric | old | new |\n|---|---|---|\n");
        let metric = |f: &dyn Fn(&[Output]) -> String| -> Vec<String> {
            results.iter().map(|(_, outs)| f(outs)).collect()
        };
        let rows: Vec<(&str, Vec<String>)> = vec![
            (
                "generation time (s, per run)",
                metric(&|o| {
                    o.iter()
                        .map(|x| format!("{:.0}", x.seconds))
                        .collect::<Vec<_>>()
                        .join(" / ")
                }),
            ),
            ("model calls", metric(&|o| o[0].calls.to_string())),
            ("output chars", metric(&|o| o[0].markdown.len().to_string())),
            (
                "output words",
                metric(&|o| o[0].markdown.split_whitespace().count().to_string()),
            ),
            (
                "## sections",
                metric(&|o| {
                    o[0].markdown
                        .lines()
                        .filter(|l| l.starts_with("## "))
                        .count()
                        .to_string()
                }),
            ),
            (
                "action items",
                metric(&|o| section_items(&o[0].markdown, "Action items").to_string()),
            ),
            (
                "decisions",
                metric(&|o| section_items(&o[0].markdown, "Decisions").to_string()),
            ),
            (
                "[mm:ss] links (resolving)",
                metric(&|o| {
                    let (t, r) = links(&o[0].markdown, &segments);
                    format!("{t} ({r})")
                }),
            ),
            (
                "identical across runs",
                metric(&|o| {
                    o.windows(2)
                        .all(|w| w[0].markdown == w[1].markdown)
                        .to_string()
                }),
            ),
        ];
        for (label, values) in rows {
            let _ = writeln!(report, "| {label} | {} |", values.join(" | "));
        }
        for (variant, outputs) in &results {
            let _ = writeln!(
                report,
                "\n## Output ({variant}, run 1)\n\n````markdown\n{}\n````",
                outputs[0].markdown
            );
            for (i, other) in outputs.iter().enumerate().skip(1) {
                if other.markdown != outputs[0].markdown {
                    let _ = writeln!(
                        report,
                        "\n### Output ({variant}, run {})\n\n````markdown\n{}\n````",
                        i + 1,
                        other.markdown
                    );
                }
            }
        }
        let path = dir.join("enhanced-ab.md");
        std::fs::write(&path, &report).expect("write the report");
        println!("{report}\nreport: {}", path.display());
    }
    crate::summary::summary_engine::client::shutdown_sidecar_gracefully()
        .await
        .ok();
}
