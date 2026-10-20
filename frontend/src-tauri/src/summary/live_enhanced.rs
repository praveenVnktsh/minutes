//! The model calls of the Enhanced notes built from the live notes (see
//! `live_outline` for the shape of the notes): one section per topic run,
//! written in the background while the meeting records
//! (`live_summary::worker`), and at stop the last run, anything missing and
//! the title (`finish_notes`, from the summary job). Re-enhance writes every
//! section again from the current transcript.
//!
//! Calls go to the live notes model (Gemma 4 E2B once downloaded, else the
//! summary model) with the live notes' deterministic settings: low
//! temperature and the fixed seed.

use async_trait::async_trait;
use log::{info, warn};
use tokio_util::sync::CancellationToken;

use crate::live_summary::llm::{GenerateError, Generator};
use crate::summary::live_outline::{
    assemble, fallback_reply, parse_section_reply, parse_transcript, place_user_notes,
    section_body, section_system_prompt, section_user_prompt, title_source, topic_runs,
    user_notes_in, LiveNote, TopicRun, TranscriptLine, UserNote, WrittenSection, SECTION_MAX_LINES,
};
use crate::summary::processor::{
    build_title_user_prompt, clean_llm_markdown_detailed, sanitize_generated_title,
    title_system_prompt,
};

/// Writes one run's section: its notes, its transcript slice and the user's
/// notes typed during it, to at most four bullets plus open questions. An
/// answer with no usable bullet keeps the run's live notes as its bullets.
pub async fn write_section(
    generator: &dyn Generator,
    run: &TopicRun,
    transcript: &[TranscriptLine],
    user_notes: &[UserNote],
    cancel: &CancellationToken,
) -> Result<WrittenSection, GenerateError> {
    let lines = run.lines(transcript);
    let users = user_notes_in(user_notes, run.start_ms, run.end_ms);
    let prompt = section_user_prompt(run, &users, &lines);
    let generated = generator
        .generate_section(section_system_prompt(), &prompt, SECTION_MAX_LINES, cancel)
        .await?;
    let mut text = clean_llm_markdown_detailed(&generated.text).markdown;
    if generated.truncated {
        // The last line may be cut mid-word.
        if let Some(cut) = text.trim_end().rfind('\n') {
            text.truncate(cut);
        }
    }
    let mut reply = parse_section_reply(&text, (run.start_ms, run.end_ms), &lines, &users);
    if reply.bullets.is_empty() {
        reply.bullets = fallback_reply(run).bullets;
    }
    Ok(WrittenSection {
        start_ms: run.start_ms,
        title: run.title.clone(),
        body: section_body(&users, &reply),
        questions: reply.questions_text(),
    })
}

/// A run's section without a model: the user's notes and its live notes.
pub fn unwritten_section(run: &TopicRun, user_notes: &[UserNote]) -> WrittenSection {
    let users = user_notes_in(user_notes, run.start_ms, run.end_ms);
    WrittenSection {
        start_ms: run.start_ms,
        title: run.title.clone(),
        body: section_body(&users, &fallback_reply(run)),
        questions: String::new(),
    }
}

/// The meeting's title from its topics and notes; None when the model gives
/// nothing usable.
pub async fn write_title(
    generator: &dyn Generator,
    runs: &[TopicRun],
    user_notes: &str,
    cancel: &CancellationToken,
) -> Option<String> {
    let prompt = build_title_user_prompt(&title_source(runs), Some(user_notes));
    match generator
        .generate_section(title_system_prompt(), &prompt, 2, cancel)
        .await
    {
        Ok(generated) => sanitize_generated_title(&generated.text),
        Err(error) => {
            warn!("Enhanced notes: the title call failed ({error})");
            None
        }
    }
}

/// Where finished sections are stored as they are written.
#[async_trait]
pub trait SectionSink: Send + Sync {
    async fn save(&self, section: &WrittenSection, end_ms: i64);
}

/// The notes, and how many model calls they took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedNotes {
    pub markdown: String,
    pub calls: usize,
    /// The notes document's wrap-up, when the tidy pass wrote the notes.
    pub tidy: Option<crate::summary::tidy::TidyWrapUp>,
}

/// Everything the finished notes are made from.
pub struct NotesInputs<'a> {
    pub notes: &'a [LiveNote],
    pub transcript_text: &'a str,
    pub user_notes: Vec<UserNote>,
    /// The user's notes as Markdown, for the title call.
    pub user_notes_markdown: &'a str,
    /// Sections already written (during the meeting).
    pub stored: Vec<WrittenSection>,
    /// Re-enhance: write every section again.
    pub regenerate: bool,
}

/// The finished Enhanced notes: sections still missing (at stop, the last
/// topic) are written and stored one by one, then the title, then the rest
/// is put together without a model. A section call that fails keeps the
/// run's live notes.
pub async fn finish_notes(
    generator: &dyn Generator,
    inputs: NotesInputs<'_>,
    sink: &dyn SectionSink,
    cancel: &CancellationToken,
) -> Result<FinishedNotes, String> {
    let (lines, _) = parse_transcript(inputs.transcript_text);
    let end_ms = lines.iter().map(|l| l.ms).max().unwrap_or(0);
    let runs = topic_runs(inputs.notes, end_ms);
    let user_notes = place_user_notes(&runs, &lines, inputs.user_notes);
    let mut written: Vec<WrittenSection> = if inputs.regenerate {
        Vec::new()
    } else {
        inputs
            .stored
            .into_iter()
            .filter(|s| {
                runs.iter()
                    .any(|r| r.start_ms == s.start_ms && r.title == s.title)
            })
            .collect()
    };
    let missing: Vec<&TopicRun> = runs
        .iter()
        .filter(|r| !r.is_small() && !written.iter().any(|s| s.start_ms == r.start_ms))
        .collect();
    info!(
        "Enhanced notes from live notes: {} topics, {} sections written during the meeting, {} to write",
        runs.len(),
        written.len(),
        missing.len()
    );
    let mut calls = 0;
    for run in missing {
        if cancel.is_cancelled() {
            return Err("Summary generation was cancelled".to_string());
        }
        let section = match write_section(generator, run, &lines, &user_notes, cancel).await {
            Ok(section) => {
                calls += 1;
                section
            }
            Err(_) if cancel.is_cancelled() => {
                return Err("Summary generation was cancelled".to_string())
            }
            Err(error) => {
                warn!("Enhanced notes: a section call failed ({error}); keeping its live notes");
                unwritten_section(run, &user_notes)
            }
        };
        sink.save(&section, run.end_ms).await;
        written.push(section);
    }
    let (title, title_calls) =
        meeting_title(generator, &runs, inputs.user_notes_markdown, cancel).await;
    calls += title_calls;
    if cancel.is_cancelled() {
        return Err("Summary generation was cancelled".to_string());
    }
    Ok(FinishedNotes {
        markdown: assemble(&title, &runs, &written, inputs.notes, &lines),
        calls,
        tidy: None,
    })
}

/// The meeting's title, or its first written topic when the model gives
/// nothing usable; and the calls it took.
async fn meeting_title(
    generator: &dyn Generator,
    runs: &[TopicRun],
    user_notes_markdown: &str,
    cancel: &CancellationToken,
) -> (String, usize) {
    match write_title(generator, runs, user_notes_markdown, cancel).await {
        Some(title) => (title, 1),
        None => (
            runs.iter()
                .find(|r| !r.is_small())
                .or(runs.first())
                .map_or_else(|| "Meeting notes".to_string(), |r| r.title.clone()),
            0,
        ),
    }
}

/// The notes from the tidy pass (see `summary::tidy`) and a title, or
/// `None` when the pass does not run or its answer is not kept: the notes
/// are then written as before.
pub async fn tidy_notes(
    tidy_model: &dyn crate::summary::tidy::TidyModel,
    generator: &dyn Generator,
    inputs: &NotesInputs<'_>,
    cancel: &CancellationToken,
) -> Option<FinishedNotes> {
    let (lines, _) = parse_transcript(inputs.transcript_text);
    let end_ms = lines
        .iter()
        .map(|l| l.ms)
        .chain(inputs.notes.iter().map(|n| n.end_ms))
        .max()
        .unwrap_or(0);
    let runs = topic_runs(inputs.notes, end_ms);
    let user_notes = place_user_notes(&runs, &lines, inputs.user_notes.clone());
    let tidy = crate::summary::tidy::run(
        tidy_model,
        crate::summary::tidy::TidyInputs {
            runs: &runs,
            user_notes: &user_notes,
            transcript_text: inputs.transcript_text,
            end_ms,
        },
        cancel,
    )
    .await?;
    let (title, title_calls) =
        meeting_title(generator, &runs, inputs.user_notes_markdown, cancel).await;
    if cancel.is_cancelled() {
        return None;
    }
    Some(FinishedNotes {
        markdown: format!("# {title}\n\n{}\n", tidy.markdown),
        calls: 1 + title_calls,
        tidy: Some(tidy.wrap_up),
    })
}

/// How long the summary at stop waits for the live summary worker to finish
/// its last window and the section it was writing.
pub const SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Why a meeting's notes are written the old way (the whole transcript).
pub fn old_path_reason(
    notes: &[LiveNote],
    transcript_text: &str,
    has_prompt_override: bool,
    language: Option<&str>,
) -> Option<String> {
    if has_prompt_override {
        return Some("a custom summary prompt is saved".to_string());
    }
    if language.is_some_and(|code| !code.trim().to_lowercase().starts_with("en")) {
        return Some("the notes are not in English".to_string());
    }
    let (lines, timed) = parse_transcript(transcript_text);
    crate::summary::live_outline::check(notes, &lines, timed)
        .err()
        .map(|reason| reason.to_string())
}

/// Whether the built-in summary model is the live notes model (then both
/// run on the one helper).
async fn summary_model_is_live_notes_model(pool: &sqlx::SqlitePool) -> bool {
    use crate::database::repositories::setting::SettingsRepository;
    use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
    SettingsRepository::get_model_config(pool)
        .await
        .ok()
        .flatten()
        .is_some_and(|settings| {
            crate::live_summary::llm::is_builtin(&settings.provider)
                && settings.model == LIVE_NOTES_MODEL
        })
}

struct DbSink<'a> {
    pool: &'a sqlx::SqlitePool,
    meeting_id: &'a str,
    provider: String,
    model: String,
}

#[async_trait]
impl SectionSink for DbSink<'_> {
    async fn save(&self, section: &WrittenSection, end_ms: i64) {
        use crate::database::repositories::enhanced_sections::{
            EnhancedSectionRow, EnhancedSectionsRepository,
        };
        let row = EnhancedSectionRow {
            meeting_id: self.meeting_id.to_string(),
            start_ms: section.start_ms,
            end_ms,
            title: section.title.clone(),
            body: section.body.clone(),
            questions: section.questions.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(error) = EnhancedSectionsRepository::upsert(self.pool, &row).await {
            warn!("Enhanced notes: could not store a section: {error}");
        }
    }
}

/// The user's notes for the meeting: the stored document (with the time
/// each note was typed), else the Markdown the summary was given.
async fn meeting_user_notes(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    markdown: &str,
) -> Vec<UserNote> {
    let stored = sqlx::query_scalar::<_, String>(
        "SELECT notes_json FROM meeting_notes WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|json| serde_json::from_str::<crate::live_notes::LiveNotesDocument>(&json).ok());
    match stored {
        Some(document) => document.user_notes(),
        None => crate::summary::live_outline::parse_user_notes(markdown),
    }
}

/// The meeting's Enhanced notes from its live notes, or `Ok(None)` when the
/// old path should write them (the reason is logged). `regenerate` is
/// Re-enhance: every section is written again.
#[allow(clippy::too_many_arguments)]
pub async fn notes_from_live_notes<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    transcript_text: &str,
    user_notes_markdown: &str,
    language: Option<&str>,
    regenerate: bool,
    cancel: &CancellationToken,
) -> Result<Option<FinishedNotes>, String> {
    use crate::database::repositories::enhanced_sections::EnhancedSectionsRepository;
    use crate::database::repositories::timeline::TimelineRepository;
    use crate::summary::prompt_settings::{load_prompt, PromptKind};
    use tauri::Manager;

    // The worker's last window and section land before the notes are read.
    crate::live_summary::settle_meeting(meeting_id, SETTLE_TIMEOUT).await;
    let notes: Vec<LiveNote> = TimelineRepository::list_for_meeting(pool, meeting_id)
        .await
        .map_err(|e| format!("Could not load the live notes: {e}"))?
        .iter()
        .map(LiveNote::from)
        .collect();
    let has_override = load_prompt(app, PromptKind::Notes).is_some()
        || load_prompt(app, PromptKind::TranscriptOnly).is_some();
    if let Some(reason) = old_path_reason(&notes, transcript_text, has_override, language) {
        info!("Enhanced notes for {meeting_id} from the whole transcript: {reason}");
        return Ok(None);
    }
    let generator = crate::live_summary::llm::SummaryModelGenerator::from_settings(
        pool,
        app.path().app_data_dir().ok(),
        crate::live_summary::model_choice::choice(app),
    )
    .await
    .map_err(|e| e.to_string())?;
    let stored = EnhancedSectionsRepository::list_for_meeting(pool, meeting_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|row| WrittenSection {
            start_ms: row.start_ms,
            title: row.title,
            body: row.body,
            questions: row.questions,
        })
        .collect();
    let user_notes = meeting_user_notes(pool, meeting_id, user_notes_markdown).await;
    let sink = DbSink {
        pool,
        meeting_id,
        provider: generator.provider().to_string(),
        model: generator.model().to_string(),
    };
    let inputs = NotesInputs {
        notes: &notes,
        transcript_text,
        user_notes,
        user_notes_markdown,
        stored,
        regenerate,
    };
    // The tidy pass, when its adapter and model are on disk.
    if let Some((app_data_dir, adapter)) = app
        .path()
        .app_data_dir()
        .ok()
        .and_then(|dir| crate::summary::tidy::available(&dir).map(|adapter| (dir, adapter)))
    {
        info!(
            "Enhanced notes for {meeting_id}: tidy pass with {}",
            adapter.display()
        );
        // E2B runs on its own helper unless it is the summary model.
        let own_helper = !summary_model_is_live_notes_model(pool).await;
        let tidy_model = crate::summary::tidy::BuiltinTidyModel {
            app_data_dir,
            adapter,
            own_helper,
        };
        if let Some(finished) = tidy_notes(&tidy_model, &generator, &inputs, cancel).await {
            return Ok(Some(finished));
        }
        if cancel.is_cancelled() {
            return Err("Summary generation was cancelled".to_string());
        }
    }
    info!(
        "Enhanced notes for {meeting_id} from {} live notes with {}",
        notes.len(),
        generator.model()
    );
    finish_notes(&generator, inputs, &sink, cancel)
        .await
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_summary::llm::Generated;
    use crate::summary::live_outline::format_transcript_clock;
    use std::sync::Mutex;

    /// Answers by the request: titles, or bullets naming the topic.
    #[derive(Default)]
    struct FakeGenerator {
        prompts: Mutex<Vec<String>>,
        fail_on: Option<&'static str>,
    }

    #[async_trait]
    impl Generator for FakeGenerator {
        fn provider(&self) -> &str {
            "builtin-ai"
        }
        fn model(&self) -> &str {
            "gemma4:e2b"
        }
        async fn generate(
            &self,
            system: &str,
            user: &str,
            _: &CancellationToken,
        ) -> Result<Generated, GenerateError> {
            self.prompts.lock().unwrap().push(user.to_string());
            if system == title_system_prompt() {
                return Ok(Generated {
                    text: "Pricing and Hiring".into(),
                    truncated: false,
                });
            }
            if self.fail_on.is_some_and(|topic| user.contains(topic)) {
                return Err(GenerateError::transient("server error"));
            }
            let title = user
                .lines()
                .next()
                .unwrap_or("")
                .trim_start_matches("Part of the meeting: ")
                .split(" (")
                .next()
                .unwrap_or("")
                .to_string();
            Ok(Generated {
                text: format!("- Written bullet about {title} here [00:30]"),
                truncated: false,
            })
        }
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<(i64, i64)>>);

    #[async_trait]
    impl SectionSink for Sink {
        async fn save(&self, section: &WrittenSection, end_ms: i64) {
            self.0.lock().unwrap().push((section.start_ms, end_ms));
        }
    }

    fn note(min: i64, text: &str, topic: &str) -> LiveNote {
        LiveNote::new(
            min * 60_000,
            min * 60_000 + 10_000,
            "point",
            text,
            None,
            Some(topic),
        )
    }

    fn transcript() -> String {
        (0..20)
            .map(|i| {
                format!(
                    "[{}] [Others] line {i}",
                    format_transcript_clock(i * 30_000)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn notes() -> Vec<LiveNote> {
        vec![
            note(0, "Pricing at forty per seat", "Pricing"),
            note(1, "Annual plan first", "Pricing"),
            note(2, "Three year discount", "Pricing"),
            note(5, "Two engineers to hire", "Hiring"),
            note(6, "Backend role first", "Hiring"),
            note(7, "Interviews next week", "Hiring"),
        ]
    }

    #[tokio::test]
    async fn at_stop_only_the_missing_sections_and_the_title_are_written() {
        let generator = FakeGenerator::default();
        let sink = Sink::default();
        let stored = vec![WrittenSection {
            start_ms: 0,
            title: "Pricing".into(),
            body: "- Stored pricing bullet [00:30]".into(),
            questions: String::new(),
        }];
        let finished = finish_notes(
            &generator,
            NotesInputs {
                notes: &notes(),
                transcript_text: &transcript(),
                user_notes: vec![UserNote {
                    ms: Some(330_000),
                    text: "ask about remote".into(),
                }],
                user_notes_markdown: "- ask about remote",
                stored,
                regenerate: false,
            },
            &sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(finished.calls, 2, "the Hiring section and the title");
        assert_eq!(*sink.0.lock().unwrap(), [(300_000, 570_001)]);
        let prompts = generator.prompts.lock().unwrap();
        assert!(prompts[0].starts_with("Part of the meeting: Hiring"));
        assert!(prompts[0].contains("do not repeat):\n- ask about remote"));
        assert_eq!(
            finished.markdown,
            "# Pricing and Hiring\n\n## Pricing\n\n- Stored pricing bullet [00:30]\n\n## Hiring\n\n- ask about remote\n- Written bullet about Hiring here [05:00]\n"
        );
    }

    struct Tidy(String);

    #[async_trait]
    impl crate::summary::tidy::TidyModel for Tidy {
        async fn generate(
            &self,
            _: &str,
            user: &str,
            _: &CancellationToken,
        ) -> Result<(String, bool), String> {
            assert!(
                user.starts_with("MEETING LENGTH: 10 min\nLIVE NOTES:\n## Pricing (from 00:00)\n")
            );
            Ok((self.0.clone(), false))
        }
    }

    const TIDY_ANSWER: &str = "## Summary
Pricing and hiring review: the plan is forty per seat, annual first, and two engineers to hire with interviews next week.

## Decisions
- Annual plan first

## Action items
- **Team**: Run the interviews next week

## Pricing
- Per-seat pricing; annual plan first

## Hiring
- Engineers to hire; backend role first";

    fn tidy_inputs<'a>(notes: &'a [LiveNote], transcript: &'a str) -> NotesInputs<'a> {
        NotesInputs {
            notes,
            transcript_text: transcript,
            user_notes: Vec::new(),
            user_notes_markdown: "",
            stored: Vec::new(),
            regenerate: false,
        }
    }

    #[tokio::test]
    async fn the_tidy_pass_writes_the_notes_under_a_title() {
        let generator = FakeGenerator::default();
        let (notes, transcript) = (notes(), transcript());
        let finished = tidy_notes(
            &Tidy(TIDY_ANSWER.into()),
            &generator,
            &tidy_inputs(&notes, &transcript),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            finished.markdown,
            format!("# Pricing and Hiring\n\n{TIDY_ANSWER}\n")
        );
        assert_eq!(finished.calls, 2);
        let wrap_up = finished.tidy.unwrap();
        assert_eq!(wrap_up.decisions, ["Annual plan first"]);
        assert_eq!(
            wrap_up.action_items,
            ["Team — Run the interviews next week"]
        );
    }

    #[tokio::test]
    async fn a_rejected_tidy_answer_leaves_the_notes_to_the_sections() {
        let generator = FakeGenerator::default();
        let (notes, transcript) = (notes(), transcript());
        let broken = TIDY_ANSWER.replace("## Decisions", "## Choices");
        assert!(tidy_notes(
            &Tidy(broken),
            &generator,
            &tidy_inputs(&notes, &transcript),
            &CancellationToken::new(),
        )
        .await
        .is_none());
        assert!(
            generator.prompts.lock().unwrap().is_empty(),
            "no title call"
        );
    }

    #[tokio::test]
    async fn re_enhance_writes_every_section_and_a_failed_one_keeps_its_notes() {
        let generator = FakeGenerator {
            fail_on: Some("Part of the meeting: Pricing"),
            ..Default::default()
        };
        let sink = Sink::default();
        let finished = finish_notes(
            &generator,
            NotesInputs {
                notes: &notes(),
                transcript_text: &transcript(),
                user_notes: Vec::new(),
                user_notes_markdown: "",
                stored: vec![WrittenSection {
                    start_ms: 0,
                    title: "Pricing".into(),
                    body: "- Old".into(),
                    questions: String::new(),
                }],
                regenerate: true,
            },
            &sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(sink.0.lock().unwrap().len(), 2);
        assert!(finished.markdown.contains(
            "## Pricing\n\n- Pricing at forty per seat [00:00]\n- Annual plan first [01:00]\n- Three year discount [02:00]\n"
        ));
        assert!(!finished.markdown.contains("- Old"));
    }
}
