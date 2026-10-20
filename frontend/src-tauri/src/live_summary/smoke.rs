//! Manual smoke test: runs a sample meeting through the window, prompt and
//! parse path with the real built-in model, and prints latency per window.
//!
//! ```text
//! MEETILY_LLAMA_HELPER=target/release/llama-helper \
//!   cargo test -p meetily live_summary_smoke -- --ignored --nocapture
//! ```
//!
//! `MEETILY_SMOKE_APP_DATA` (default: the macOS app data folder) and
//! `MEETILY_SMOKE_MODEL` (default `qwen3.5:4b`) pick the model.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::llm::generation_options;
use super::notes::NoteContext;
use super::parse::parse_notes;
use super::prompt::{self, PromptContext};
use super::scheduler::{Segment, Speaker, WindowScheduler, CADENCE};
use super::worker::{NotesSoFar, TimelineEntry};
use crate::summary::llm_client::{generate_summary, LLMProvider};

/// (seconds, speaker, text): a synthetic five-minute planning call.
const SAMPLE: &[(u64, Speaker, &str)] = &[
    (2, Speaker::You, "Okay, thanks everyone for joining. Today I want to go through the launch plan for the mobile app and settle the date."),
    (11, Speaker::Others, "Sure. From the engineering side we are mostly done, the only open item is the offline sync bug that QA found on Tuesday."),
    (24, Speaker::Others, "Yeah and on design we still owe the onboarding screens, we were waiting on the copy from marketing."),
    (35, Speaker::You, "Right. So how long do we think the sync fix takes? Is that days or is that a week?"),
    (43, Speaker::Others, "I'd say three days including review. Ravi already has a branch, he just needs to add tests for the conflict cases."),
    (58, Speaker::You, "Good. And the copy, I can chase that today, I'll ping Dana after this call and get it to you by Thursday."),
    (70, Speaker::Others, "That works. If we get copy Thursday we can have the screens ready Monday."),
    (82, Speaker::You, "So realistically the launch moves from the third to the tenth. Does anyone object to the tenth?"),
    (91, Speaker::Others, "No, the tenth is fine for us. Support also asked for at least a week of notice for the help center articles."),
    (104, Speaker::Others, "Agreed, the tenth. Let's lock it."),
    (110, Speaker::You, "Great, the tenth it is. I'll send the announcement to support today so they have the full week."),
    (125, Speaker::Others, "One more thing, pricing. Are we still doing the free tier at launch or holding it back?"),
    (136, Speaker::You, "My preference is to hold it back for a month, we don't have the usage limits built yet and I don't want to promise something we can't meter."),
    (152, Speaker::Others, "That makes sense to me, the metering work is at least two sprints out anyway."),
    (164, Speaker::Others, "Finance will want to see the numbers though, can someone put together a quick model of paid only versus freemium?"),
    (176, Speaker::You, "I'll ask Priya, she built the last one. Let's say by end of next week."),
    (190, Speaker::Others, "Okay. Then the last topic is the beta feedback, we had about two hundred responses."),
    (201, Speaker::Others, "The biggest complaint by far is battery drain during background sync, around forty percent mention it."),
    (214, Speaker::You, "Hmm, is that the same root cause as the offline sync bug or something separate?"),
    (224, Speaker::Others, "Probably related, the retry loop wakes up too often. Ravi can check while he's in there."),
    (238, Speaker::You, "Okay so Ravi looks at the retry interval as part of the sync fix. Let's make that explicit in the ticket."),
    (252, Speaker::Others, "Will do. Second biggest complaint is that people can't find the export button."),
    (263, Speaker::Others, "We could move it into the share menu, that's a small change, maybe half a day."),
    (275, Speaker::You, "Let's do it, it's cheap. Okay, I think that's everything. Thanks all, I'll send notes after."),
];

#[tokio::test]
#[ignore = "needs a downloaded built-in model and a release llama-helper"]
async fn live_summary_smoke() {
    let app_data: PathBuf = std::env::var("MEETILY_SMOKE_APP_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap())
                .join("Library/Application Support/com.meetily.ai")
        });
    let model = std::env::var("MEETILY_SMOKE_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into());
    println!("model {model} from {}", app_data.display());

    // Replay the transcript on audio time: a window is due every CADENCE.
    let mut scheduler = WindowScheduler::new(Duration::ZERO);
    let mut kept: Vec<TimelineEntry> = Vec::new();
    let mut carryover: Vec<Segment> = Vec::new();
    let mut latencies = Vec::new();
    let end = SAMPLE.last().unwrap().0 + 10;
    let mut samples = SAMPLE.iter().peekable();
    for second in 0..=end + CADENCE.as_secs() {
        while let Some((at, speaker, text)) = samples.peek() {
            if *at > second {
                break;
            }
            scheduler.push(Segment {
                start_ms: *at as i64 * 1000,
                end_ms: *at as i64 * 1000 + 6000,
                speaker: *speaker,
                text: text.to_string(),
                received_at: Duration::from_secs(*at),
            });
            samples.next();
        }
        let now = Duration::from_secs(second);
        let window = if second > end {
            scheduler.flush(now)
        } else {
            scheduler.poll(now, false)
        };
        let Some(window) = window else { continue };

        let notes = NotesSoFar::of(&kept);
        let glossary = notes.glossary.terms();
        let user = prompt::user_prompt(
            &window,
            &PromptContext {
                current_topic: notes.current_topic.as_deref(),
                previous: &notes.previous,
                glossary: &glossary,
                carryover: &carryover,
                decided_topic: None,
            },
        );
        let started = Instant::now();
        let raw = generate_summary(
            &reqwest::Client::new(),
            &LLMProvider::BuiltInAI,
            &model,
            "",
            prompt::system_prompt(),
            &user,
            None,
            None,
            &generation_options(),
            Some(&app_data),
            Some(&CancellationToken::new()),
        )
        .await
        .map(|completion| completion.content);
        let elapsed = started.elapsed();
        scheduler.complete();
        latencies.push(elapsed);
        println!(
            "\nwindow {}..{} ({} words): {:.2}s",
            prompt::format_clock(window.start_ms),
            prompt::format_clock(window.end_ms),
            window.words(),
            elapsed.as_secs_f64()
        );
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                println!("  request failed: {error:#}");
                continue;
            }
        };
        let history: Vec<String> = kept.iter().rev().take(10).map(|e| e.text.clone()).collect();
        let context = NoteContext {
            current_topic: notes.current_topic.as_deref(),
            known_topics: &notes.topics,
            glossary: Some(&notes.glossary),
            previous_texts: &history,
            figures: None,
            decided_topic: None,
            drop_fragments: false,
            segments: &window.segments,
        };
        match parse_notes(
            &raw,
            window.start_ms,
            window.end_ms,
            window.max_entries,
            &context,
        ) {
            Ok(entries) => {
                carryover = window.segments.clone();
                for entry in entries {
                    println!(
                        "  [{}] {:<8} {} | {} ({})",
                        prompt::format_clock(entry.start_ms),
                        entry.kind.as_str(),
                        entry.topic.as_deref().unwrap_or("-"),
                        entry.text,
                        entry.speaker.as_deref().unwrap_or("-")
                    );
                    kept.push(TimelineEntry {
                        id: kept.len().to_string(),
                        start_ms: entry.start_ms,
                        end_ms: entry.end_ms,
                        kind: entry.kind.as_str().into(),
                        text: entry.text,
                        speaker: entry.speaker,
                        provider: "builtin-ai".into(),
                        model: model.clone(),
                        created_at: String::new(),
                        topic: entry.topic,
                        parent: None,
                    });
                }
            }
            Err(error) => println!("  parse failed ({error}): {raw:?}"),
        }
    }
    let total: Duration = latencies.iter().sum();
    println!(
        "\n{} windows, mean {:.2}s, max {:.2}s (first includes model load)",
        latencies.len(),
        total.as_secs_f64() / latencies.len().max(1) as f64,
        latencies
            .iter()
            .max()
            .copied()
            .unwrap_or_default()
            .as_secs_f64()
    );
    crate::summary::summary_engine::client::shutdown_sidecar_gracefully()
        .await
        .ok();
}
