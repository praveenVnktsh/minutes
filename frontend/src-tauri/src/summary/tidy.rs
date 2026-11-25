//! The end-of-meeting tidy pass: one call on Gemma 4 E2B with the tidy
//! adapter, from the meeting's live notes and its whole transcript, to the
//! final notes (Summary, Decisions, Action items, 3–7 subject sections, Open
//! questions). It runs in the summary job at stop when the adapter and the
//! model are on disk; its answer is checked and, when it passes, becomes the
//! stored Enhanced notes, and its Summary, Decisions, Action items and Open
//! questions become the notes document's wrap-up (`completeAiNotes`).
//! Anything else (no adapter, a failed call, an answer the checker rejects)
//! leaves the notes exactly as they were written before the tidy pass.
//!
//! Ported from minutes-distill `scripts/tidy_data.py`, the adapter's training
//! source: `SYSTEM_PROMPT`, `build_input` (byte for byte, see the golden
//! tests) and the hard errors of `check`. Its warnings are not ported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use log::{info, warn};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::live_summary::facts::Figures;
use crate::summary::live_outline::{
    format_transcript_clock, parse_transcript_clock, user_notes_in, LiveNote, NoteKind, TopicRun,
    UserNote,
};

/// The system prompt the tidy adapter was trained with (`tidy_data.py`).
pub const SYSTEM_PROMPT: &str = "Meeting summariser. Given the live notes taken during a meeting ([you] = the user's own notes, [ai] = live AI notes, which can be wrong, duplicated or badly grouped) and the full transcript, write the final notes in markdown:
## Summary  2-3 sentences: what the meeting was for and what came out of it
## Decisions  - one bullet per decision actually agreed (\"- None\" if none)
## Action items  - **Owner**: task (deadline if said) (\"- None\" if none)
## <Subject>  3-7 sections by subject, in meeting order: headline bullets, outcome and figures first, \"  - \" sub-bullets for detail
## Open questions  only if something was left unresolved
Use only facts from the transcript; keep every point of the user's notes; merge duplicates; fix wrong live notes; no filler, no narration of who said what.";

/// Output budget (`tidy_generate.py`: max_tokens 1400, greedy).
pub const MAX_TOKENS: u32 = 1_400;
/// The context the call runs with.
pub const CONTEXT_SIZE: u32 = 20_480;
/// A prompt estimated above this is not sent: it would not fit the context
/// with the answer's room (the estimate is generous, see
/// `rough_token_count`).
const MAX_PROMPT_TOKENS: usize = (CONTEXT_SIZE - MAX_TOKENS) as usize - 512;
/// A 1400-token answer on E2B takes well under a minute; the long prefill of
/// a full meeting is the rest.
const TIMEOUT: Duration = Duration::from_secs(240);

const SECTIONS_FIXED: [&str; 3] = ["Summary", "Decisions", "Action items"];
const OPEN_QUESTIONS: &str = "Open questions";

/// One topic of the live notes, as the adapter sees them
/// (`notes.json` of minutes-distill's closed loop).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TidySection {
    pub topic: Option<String>,
    /// `mm:ss` the topic started.
    pub since: Option<String>,
    pub notes: Vec<TidyNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TidyNote {
    /// A detail under the note above.
    pub child: bool,
    /// `you` (the user's own note) or `ai`.
    pub by: String,
    /// `point`, `decision` or `action`.
    pub kind: String,
    pub owner: Option<String>,
    pub text: String,
}

/// `render_notes`.
pub fn render_notes(sections: &[TidySection]) -> Vec<String> {
    let mut lines = Vec::new();
    for section in sections {
        let topic = section
            .topic
            .as_deref()
            .filter(|t| !t.is_empty())
            .unwrap_or("Notes");
        let since = section
            .since
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" (from {s})"))
            .unwrap_or_default();
        lines.push(format!("## {topic}{since}"));
        for note in &section.notes {
            let kind = if note.kind == "point" {
                String::new()
            } else {
                let owner = note
                    .owner
                    .as_deref()
                    .filter(|o| !o.is_empty())
                    .map(|o| format!(", {o}"))
                    .unwrap_or_default();
                format!(" ({}{owner})", note.kind)
            };
            let indent = if note.child { "  " } else { "" };
            lines.push(format!("{indent}- [{}] {}{kind}", note.by, note.text));
        }
    }
    lines
}

/// Python's `round(ms / 60000)`: halves go to the even minute.
pub fn meeting_minutes(end_ms: i64) -> i64 {
    let end_ms = end_ms.max(0);
    let (whole, rest) = (end_ms / 60_000, end_ms % 60_000);
    match (rest * 2).cmp(&60_000) {
        std::cmp::Ordering::Greater => whole + 1,
        std::cmp::Ordering::Equal => whole + whole % 2,
        std::cmp::Ordering::Less => whole,
    }
}

static APP_LINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\[(\d{1,3}:\d{2}(?::\d{2})?)\]\s+\[([^\]]+)\]\s?(.*)$").unwrap());

/// The summary transcript (`[mm:ss] [Speaker] text`) as the adapter's
/// transcript lines (`[mm:ss] Speaker: text`, speakers `You` and `Others`
/// only, as in training), a repeated line once (`transcript_lines`).
/// Lines without a time or a speaker are left out.
pub fn transcript_lines(transcript_text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in transcript_text.lines().map(str::trim) {
        let Some(captures) = APP_LINE.captures(line) else {
            continue;
        };
        let Some(ms) = parse_transcript_clock(&captures[1]) else {
            continue;
        };
        let speaker = if captures[2].trim().eq_ignore_ascii_case("you") {
            "You"
        } else {
            "Others"
        };
        let text = captures[3].trim();
        let line = format!("[{}] {speaker}: {text}", format_transcript_clock(ms));
        if seen.insert(line.clone()) {
            out.push(line);
        }
    }
    out
}

/// `build_input`: the user message the adapter sees.
pub fn build_input(minutes: i64, sections: &[TidySection], transcript: &[String]) -> String {
    let mut lines = vec![
        format!("MEETING LENGTH: {minutes} min"),
        "LIVE NOTES:".to_string(),
    ];
    let notes = render_notes(sections);
    if notes.is_empty() {
        lines.push("(none)".to_string());
    } else {
        lines.extend(notes);
    }
    lines.push("TRANSCRIPT:".to_string());
    lines.extend(transcript.iter().cloned());
    lines.join("\n")
}

/// The app's live notes as the adapter's topics: one section per topic run,
/// the user's notes typed during it among its notes by time. The app keeps
/// no note hierarchy, so no note is a child.
pub fn sections_from_live_notes(
    runs: &[TopicRun],
    user_notes: &[UserNote],
    first_line_ms: Option<i64>,
) -> Vec<TidySection> {
    runs.iter()
        .enumerate()
        .map(|(index, run)| {
            let mut timed: Vec<(i64, TidyNote)> = run
                .notes
                .iter()
                .map(|note| (note.start_ms, ai_note(note)))
                .collect();
            for user in user_notes_in(user_notes, run.start_ms, run.end_ms) {
                timed.push((
                    user.ms.unwrap_or(run.start_ms),
                    TidyNote {
                        child: false,
                        by: "you".to_string(),
                        kind: "point".to_string(),
                        owner: None,
                        text: user.text.trim().to_string(),
                    },
                ));
            }
            timed.sort_by_key(|(ms, _)| *ms);
            // The first topic starts with the meeting, later ones at their
            // first note.
            let since = if index == 0 {
                let first_note = run.notes.first().map_or(0, |n| n.start_ms);
                first_line_ms.map_or(first_note, |ms| ms.min(first_note))
            } else {
                run.start_ms
            };
            TidySection {
                topic: Some(run.title.clone()),
                since: Some(format_transcript_clock(since)),
                notes: timed
                    .into_iter()
                    .map(|(_, note)| note)
                    .filter(|note| !note.text.is_empty())
                    .collect(),
            }
        })
        .filter(|section| !section.notes.is_empty())
        .collect()
}

fn ai_note(note: &LiveNote) -> TidyNote {
    let kind = match note.kind {
        NoteKind::Point => "point",
        NoteKind::Decision => "decision",
        NoteKind::Action => "action",
    };
    TidyNote {
        child: false,
        by: "ai".to_string(),
        kind: kind.to_string(),
        owner: (note.kind == NoteKind::Action)
            .then(|| note.speaker.clone())
            .flatten(),
        text: note.text.clone(),
    }
}

static HEADING: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^## (.+?)\s*$").unwrap());
static MARKUP: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[(ai|you)\]|\(from \d\d:\d\d\)|^\[\d\d:\d\d\]").unwrap());
static ACTION: Lazy<Regex> = Lazy::new(|| Regex::new(r"^- \*\*[^*]+\*\*: \S").unwrap());
static TRANSCRIPT_PREFIX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\[\d\d:\d\d\] \w+: ").unwrap());

/// The `## ` sections of an answer, in order, each with its stripped body
/// (`check`'s `body`; a heading that repeats keeps its last body).
fn sections_of(text: &str) -> Vec<(String, String)> {
    let heads: Vec<(usize, usize, String)> = HEADING
        .captures_iter(text)
        .map(|c| {
            let whole = c.get(0).unwrap();
            (whole.start(), whole.end(), c[1].to_string())
        })
        .collect();
    heads
        .iter()
        .enumerate()
        .map(|(i, (_, end, head))| {
            let next = heads.get(i + 1).map_or(text.len(), |h| h.0);
            let body = text.get(*end..next).unwrap_or("");
            (head.clone(), body.trim().to_string())
        })
        .collect()
}

fn body_of<'a>(sections: &'a [(String, String)], head: &str) -> &'a str {
    sections
        .iter()
        .rev()
        .find(|(h, _)| h == head)
        .map_or("", |(_, body)| body.as_str())
}

fn starts_with_chars(text: &str, prefix: &str, chars: usize) -> bool {
    let prefix: String = prefix.chars().take(chars).collect();
    text.starts_with(&prefix)
}

const UNITS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const CURRENCY_WORDS: [&str; 8] = [
    "euro", "euros", "pound", "pounds", "dollar", "dollars", "quid", "bucks",
];

/// A spoken number below 100 at `words[at..]`, and how many words it takes.
fn spoken_below_100(words: &[&str], at: usize) -> Option<(u32, usize)> {
    let word = *words.get(at)?;
    if let Some(unit) = UNITS.iter().position(|u| *u == word) {
        return Some((unit as u32, 1));
    }
    let tens = TENS.iter().position(|t| *t == word)? as u32 * 10 + 20;
    match words
        .get(at + 1)
        .and_then(|next| UNITS[1..10].iter().position(|u| u == next))
    {
        Some(unit) => Some((tens + unit as u32 + 1, 2)),
        None => Some((tens, 1)),
    }
}

/// Spoken prices `live_summary::facts` does not read but the training
/// checker does (`validate_labels.py`): "twelve fifty" is 12.50, and so is
/// "twelve euros fifty"; "nine point nine five" is 9.95.
fn spoken_prices(source: &str) -> Vec<f64> {
    let lower = source.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    let mut prices = Vec::new();
    for at in 0..words.len() {
        let Some((whole, used)) = spoken_below_100(&words, at) else {
            continue;
        };
        let next = at + used;
        if words.get(next) == Some(&"point") {
            // "nine point nine five": digit by digit.
            let digits: String = words[next + 1..]
                .iter()
                .map_while(|w| UNITS[..10].iter().position(|u| u == w))
                .map(|d| char::from(b'0' + d as u8))
                .collect();
            if let Ok(value) = format!("{whole}.{digits}").parse::<f64>() {
                prices.push(value);
            }
        } else if words.get(next).is_some_and(|w| CURRENCY_WORDS.contains(w)) {
            if let Some((cents, _)) = spoken_below_100(&words, next + 1) {
                prices.push(f64::from(whole) + f64::from(cents) / 100.0);
            }
        } else if whole >= 10 {
            if let Some((cents, _)) = spoken_below_100(&words, next).filter(|(c, _)| *c >= 10) {
                prices.push(f64::from(whole) + f64::from(cents) / 100.0);
            }
        }
    }
    prices
}

/// Whether a figure `Figures` rejected is one of the spoken prices.
fn spoken_price(figure: &str, prices: &[f64]) -> bool {
    let digits: String = figure
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits
        .trim_matches('.')
        .parse::<f64>()
        .is_ok_and(|value| prices.iter().any(|p| (p - value).abs() < 1e-6))
}

/// `check`'s hard errors for an answer: the sections and their order, the
/// subject count, the Summary's length, the length cap (220 words + 9 per
/// minute), copied note or transcript markup, bullets over 30 words, numbers
/// the transcript does not support, stray lines and the action format.
/// Exactly `tidy_data.py check`; the app runs it with `LENGTH_SLACK`.
#[cfg(test)]
pub fn check(text: &str, transcript: &[String], end_ms: i64) -> Vec<String> {
    check_with_slack(text, transcript, end_ms, 1.0)
}

/// What the app accepts: a few words over the length cap is still good notes
/// (2 of 10 test meetings missed it by under 6%, 2026-10-20), so the cap is
/// `LENGTH_SLACK` times `check`'s.
const LENGTH_SLACK: f64 = 1.15;

fn check_with_slack(text: &str, transcript: &[String], end_ms: i64, slack: f64) -> Vec<String> {
    let mut errors = Vec::new();
    let source = transcript
        .iter()
        .map(|line| TRANSCRIPT_PREFIX.replace(line, "").into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let minutes = meeting_minutes(end_ms).max(1);
    let heads: Vec<String> = HEADING
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect();
    if heads.len() < 3 || heads[..3] != SECTIONS_FIXED {
        errors.push(format!(
            "sections must start with {SECTIONS_FIXED:?}, got {:?}",
            &heads[..heads.len().min(3)]
        ));
    }
    let subjects = heads
        .iter()
        .skip(3)
        .filter(|h| h.as_str() != OPEN_QUESTIONS)
        .count();
    if !(2..=7).contains(&subjects) {
        errors.push(format!(
            "{subjects} subject sections (want 3-7, 2 for a very short meeting)"
        ));
    }
    if heads.iter().any(|h| h == OPEN_QUESTIONS)
        && heads.last().map(String::as_str) != Some(OPEN_QUESTIONS)
    {
        errors.push("Open questions must be the last section".to_string());
    }
    let sections = sections_of(text);
    let summary = body_of(&sections, "Summary");
    let summary_words = summary.split_whitespace().count();
    if !(15..=70).contains(&summary_words) {
        errors.push(format!("Summary has {summary_words} words (15-70)"));
    }
    let total = text
        .replace(['#', '*', '-'], " ")
        .split_whitespace()
        .count();
    let cap = 220 + 9 * minutes as usize;
    if total as f64 > cap as f64 * slack {
        errors.push(format!(
            "{total} words, over the {cap} cap for a {minutes} min meeting"
        ));
    }
    let figures = Figures::of(&source);
    let prices = spoken_prices(&source);
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("## ") {
            continue;
        }
        let short: String = t.chars().take(60).collect();
        if MARKUP.is_match(t) {
            errors.push(format!("copied note/transcript markup: {short}"));
        }
        if t.starts_with('-') {
            let item = t.trim_start_matches(['-', ' ']).trim();
            let item_short: String = item.chars().take(60).collect();
            if item.split_whitespace().count() > 30 {
                errors.push(format!("bullet over 30 words: {item_short}"));
            }
            let unsupported: Vec<String> = figures
                .unsupported(item)
                .into_iter()
                .filter(|figure| !spoken_price(figure, &prices))
                .collect();
            if !unsupported.is_empty() {
                errors.push(format!(
                    "number not in transcript ({}): {item_short}",
                    unsupported.join(",")
                ));
            }
        } else if !starts_with_chars(summary, t, 20) {
            errors.push(format!("non-bullet line outside Summary: {short}"));
        }
    }
    let actions = body_of(&sections, "Action items");
    if !actions.is_empty() && actions != "- None" {
        for t in actions.lines() {
            if t.starts_with("- ") && !ACTION.is_match(t) {
                let short: String = t.chars().take(60).collect();
                errors.push(format!("action not '- **Owner**: task': {short}"));
            }
        }
    }
    errors
}

/// The parts of the final notes the notes document's wrap-up shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TidyWrapUp {
    pub summary: String,
    pub decisions: Vec<String>,
    /// `Owner — task`, as the wrap-up has always written actions.
    pub action_items: Vec<String>,
    pub open_questions: Vec<String>,
}

/// The top-level bullets of a section, `- None` left out.
fn bullets(body: &str) -> Vec<String> {
    body.lines()
        .filter(|line| line.starts_with("- "))
        .map(|line| line[2..].trim().to_string())
        .filter(|item| !item.is_empty() && item != "None")
        .collect()
}

static OWNER: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\*\*([^*]+)\*\*:\s*(.+)$").unwrap());

/// The answer with a `- None` line dropped from any section that has other
/// bullets (the adapter sometimes writes "- None" and then the decisions).
pub fn drop_stray_none(text: &str) -> String {
    fn flush<'a>(section: &mut Vec<&'a str>, out: &mut Vec<&'a str>) {
        let others = section
            .iter()
            .any(|line| line.starts_with("- ") && line.trim() != "- None");
        out.extend(
            section
                .drain(..)
                .filter(|line| !(others && line.trim() == "- None")),
        );
    }
    let mut out: Vec<&str> = Vec::new();
    let mut section: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            flush(&mut section, &mut out);
        }
        section.push(line);
    }
    flush(&mut section, &mut out);
    out.join("\n")
}

/// The answer with each action owner the meeting never names left out
/// (`- **Owner**: task` becomes `- task`). The adapter learned owners from
/// AMI's role-labelled meetings and, when the transcript has no names, makes
/// up roles such as "Marketing expert" (2026-10-20). An owner is kept when a
/// word of it (3+ letters) is in the transcript or the user's notes; "You",
/// "Team", "Everyone" and "All" always are.
pub fn drop_unnamed_owners(text: &str, transcript: &[String], user_notes: &[UserNote]) -> String {
    let mut known = transcript
        .iter()
        .map(|line| TRANSCRIPT_PREFIX.replace(line, "").to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    for note in user_notes {
        known.push(' ');
        known.push_str(&note.text.to_lowercase());
    }
    let words: HashSet<&str> = known
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let named = |owner: &str| {
        ["you", "team", "everyone", "all"]
            .iter()
            .any(|generic| owner.eq_ignore_ascii_case(generic))
            || owner
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w.chars().count() >= 3 && words.contains(w.to_lowercase().as_str()))
    };
    text.lines()
        .map(|line| {
            match line
                .strip_prefix("- ")
                .and_then(|item| OWNER.captures(item))
            {
                Some(c) if !named(c[1].trim()) => format!("- {}", c[2].trim()),
                _ => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The wrap-up parts of checked final notes.
pub fn wrap_up(text: &str) -> TidyWrapUp {
    let sections = sections_of(text);
    TidyWrapUp {
        summary: body_of(&sections, "Summary")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        decisions: bullets(body_of(&sections, "Decisions")),
        action_items: bullets(body_of(&sections, "Action items"))
            .into_iter()
            .map(|item| match OWNER.captures(&item) {
                Some(c) => format!("{} — {}", c[1].trim(), c[2].trim()),
                None => item,
            })
            .collect(),
        open_questions: bullets(body_of(&sections, OPEN_QUESTIONS)),
    }
}

/// Checked final notes, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TidyNotes {
    /// The notes without a title.
    pub markdown: String,
    pub wrap_up: TidyWrapUp,
}

/// What the tidy pass is given.
pub struct TidyInputs<'a> {
    pub runs: &'a [TopicRun],
    pub user_notes: &'a [UserNote],
    pub transcript_text: &'a str,
    /// The meeting's end (audio time).
    pub end_ms: i64,
}

/// The model call, so the pass can be tested without one.
#[async_trait::async_trait]
pub trait TidyModel: Send + Sync {
    /// The answer, and whether it stopped at the output budget.
    async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<(String, bool), String>;
}

/// The tidy pass: the input, one call, the checker. `None` (with the reason
/// logged) when the notes should stay as they were.
pub async fn run(
    model: &dyn TidyModel,
    inputs: TidyInputs<'_>,
    cancel: &CancellationToken,
) -> Option<TidyNotes> {
    let transcript = transcript_lines(inputs.transcript_text);
    if transcript.is_empty() {
        info!("Tidy pass skipped: the transcript has no timed lines");
        return None;
    }
    let first_line_ms = inputs
        .transcript_text
        .lines()
        .find_map(|line| APP_LINE.captures(line.trim()))
        .and_then(|c| parse_transcript_clock(&c[1]));
    let sections = sections_from_live_notes(inputs.runs, inputs.user_notes, first_line_ms);
    let prompt = build_input(meeting_minutes(inputs.end_ms), &sections, &transcript);
    let estimate = crate::summary::processor::rough_token_count(SYSTEM_PROMPT)
        + crate::summary::processor::rough_token_count(&prompt);
    if estimate > MAX_PROMPT_TOKENS {
        info!("Tidy pass skipped: the meeting is too long for one call (~{estimate} tokens)");
        return None;
    }
    let started = std::time::Instant::now();
    let (answer, truncated) = match model.generate(SYSTEM_PROMPT, &prompt, cancel).await {
        Ok(answer) => answer,
        Err(error) => {
            warn!("Tidy pass failed ({error}); keeping the notes from the live notes");
            return None;
        }
    };
    if truncated {
        warn!("Tidy pass answer hit the output budget; keeping the notes from the live notes");
        return None;
    }
    let markdown = drop_stray_none(answer.trim());
    let errors = check_with_slack(&markdown, &transcript, inputs.end_ms, LENGTH_SLACK);
    if !errors.is_empty() {
        warn!(
            "Tidy pass answer rejected ({} errors, first: {}); keeping the notes from the live notes",
            errors.len(),
            errors[0]
        );
        return None;
    }
    let markdown = drop_unnamed_owners(&markdown, &transcript, inputs.user_notes);
    info!(
        "Tidy pass wrote the final notes in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    Some(TidyNotes {
        wrap_up: wrap_up(&markdown),
        markdown,
    })
}

/// The tidy adapter and the model it runs on, when both are on disk.
pub fn available(app_data_dir: &Path) -> Option<PathBuf> {
    use crate::summary::summary_engine::adapters::{adapter_path, Adapter};
    let models_dir = crate::summary::summary_engine::models::get_models_directory(app_data_dir);
    if !crate::live_summary::llm::live_notes_model_downloaded(&models_dir) {
        return None;
    }
    adapter_path(&models_dir, Adapter::Tidy)
}

/// Gemma 4 E2B with the tidy adapter, on the live notes helper (or the main
/// one when E2B is the summary model), so the summary model stays loaded.
pub struct BuiltinTidyModel {
    pub app_data_dir: PathBuf,
    pub adapter: PathBuf,
    pub own_helper: bool,
}

#[async_trait::async_trait]
impl TidyModel for BuiltinTidyModel {
    async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<(String, bool), String> {
        use crate::summary::llm_client::GenerationOptions;
        use crate::summary::summary_engine::client::{generate_live_notes, LiveNotesRequest};
        use crate::summary::summary_engine::models::LIVE_NOTES_MODEL;
        let options = GenerationOptions {
            max_tokens: Some(MAX_TOKENS),
            // Greedy, as the adapter was evaluated.
            sampling_temperature: Some(0.0),
            timeout: Some(TIMEOUT),
            lora_path: Some(self.adapter.clone()),
            context_size: Some(CONTEXT_SIZE),
            ..Default::default()
        };
        // The built-in template is Gemma 4's (`models::GEMMA4_TEMPLATE`),
        // which is the tokenizer's chat template for a system and a user
        // turn; the tokenizer trims both texts.
        let completion = generate_live_notes(
            &self.app_data_dir,
            LiveNotesRequest {
                model_name: LIVE_NOTES_MODEL,
                system_prompt: system_prompt.trim(),
                user_prompt: user_prompt.trim(),
                grammar: None,
                max_lines: None,
                own_helper: self.own_helper,
                one_off: true,
                options: (&options).into(),
            },
            Some(cancel),
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok((completion.text, completion.truncated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Golden {
        end_ms: i64,
        transcript: String,
        notes: Vec<TidySection>,
        expected: String,
    }

    fn golden(name: &str) -> Golden {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/summary/tidy_fixtures")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Fixtures from `tidy_data.build_input` on the start of two meetings
    /// (their live notes from the stage-1 model; the transcript in the app's
    /// format, with the windows' overlapping repeats).
    #[test]
    fn the_input_matches_the_training_input_byte_for_byte() {
        for name in ["ES2005a", "IS1004a"] {
            let g = golden(name);
            let transcript = transcript_lines(&g.transcript);
            let input = build_input(meeting_minutes(g.end_ms), &g.notes, &transcript);
            assert_eq!(input, g.expected, "{name}");
        }
    }

    #[test]
    fn minutes_round_like_python() {
        assert_eq!(meeting_minutes(301_010), 5);
        assert_eq!(meeting_minutes(29_999), 0);
        assert_eq!(meeting_minutes(90_000), 2, "1.5 rounds to the even 2");
        assert_eq!(meeting_minutes(150_000), 2, "2.5 rounds to the even 2");
        assert_eq!(meeting_minutes(150_001), 3);
    }

    #[test]
    fn transcript_lines_keep_you_and_others_and_drop_repeats() {
        let text = "[00:05] [You] Hi.\n[00:05] [You] Hi.\n[1:02:03] [Speaker 2] Budget is 40.\nno time here";
        assert_eq!(
            transcript_lines(text),
            ["[00:05] You: Hi.", "[62:03] Others: Budget is 40."]
        );
    }

    #[test]
    fn empty_notes_render_as_none() {
        assert_eq!(
            build_input(3, &[], &["[00:01] You: Hi.".to_string()]),
            "MEETING LENGTH: 3 min\nLIVE NOTES:\n(none)\nTRANSCRIPT:\n[00:01] You: Hi."
        );
    }

    fn live(min: i64, kind: &str, text: &str, owner: Option<&str>, topic: &str) -> LiveNote {
        LiveNote::new(
            min * 60_000,
            min * 60_000 + 5_000,
            kind,
            text,
            owner,
            Some(topic),
        )
    }

    #[test]
    fn live_notes_become_topics_with_the_users_notes_by_time() {
        let notes = [
            live(1, "point", "Price at €25", None, "Pricing"),
            live(3, "action", "Send the deck", Some("Priya"), "Pricing"),
            live(6, "decision", "Ship in May", Some("ignored"), "Launch"),
        ];
        let runs = crate::summary::live_outline::topic_runs(&notes, 8 * 60_000);
        let users = [UserNote {
            ms: Some(2 * 60_000),
            text: "pricing 25eur?".into(),
        }];
        let sections = sections_from_live_notes(&runs, &users, Some(30_000));
        assert_eq!(
            render_notes(&sections).join("\n"),
            "## Pricing (from 00:30)\n- [ai] Price at €25\n- [you] pricing 25eur?\n- [ai] Send the deck (action, Priya)\n## Launch (from 06:00)\n- [ai] Ship in May (decision)"
        );
    }

    const GOOD: &str = "## Summary
Kickoff for a new remote control that sells for 25 Euro, with production cost capped at 12.50 Euro and a clear plan for the next meeting.

## Decisions
- Selling price set at 25 Euro

## Action items
- **David**: Work on the working design
- **Team**: Read the brief

## Price and cost
- Cost capped at 12.50 Euro
  - Retailer sets the shelf price

## Features
- One remote for every device

## Open questions
- Whether to include features beyond the TV";

    fn source() -> Vec<String> {
        transcript_lines(
            "[00:10] [Others] It will sell for twenty five Euro.\n[00:20] [Others] Production cost is twelve fifty.",
        )
    }

    #[test]
    fn a_good_answer_passes_and_gives_the_wrap_up() {
        assert_eq!(check(GOOD, &source(), 300_000), Vec::<String>::new());
        assert_eq!(
            wrap_up(GOOD),
            TidyWrapUp {
                summary: "Kickoff for a new remote control that sells for 25 Euro, with production cost capped at 12.50 Euro and a clear plan for the next meeting.".into(),
                decisions: vec!["Selling price set at 25 Euro".into()],
                action_items: vec![
                    "David — Work on the working design".into(),
                    "Team — Read the brief".into()
                ],
                open_questions: vec!["Whether to include features beyond the TV".into()],
            }
        );
    }

    #[test]
    fn none_sections_give_empty_lists() {
        let text = GOOD
            .replace("- Selling price set at 25 Euro", "- None")
            .replace(
                "- **David**: Work on the working design\n- **Team**: Read the brief",
                "- None",
            );
        assert_eq!(check(&text, &source(), 300_000), Vec::<String>::new());
        let parts = wrap_up(&text);
        assert!(parts.decisions.is_empty() && parts.action_items.is_empty());
    }

    #[test]
    fn broken_answers_are_rejected() {
        let src = source();
        let cases: [(String, &str); 8] = [
            (GOOD.replacen("## Decisions", "## Choices", 1), "sections must start"),
            (GOOD.replace("## Features\n- One remote for every device\n\n", ""), "1 subject sections"),
            (GOOD.replace("## Open questions\n- Whether to include features beyond the TV", "## Open questions\n- Whether\n\n## Extra\n- More"), "Open questions must be the last"),
            (GOOD.replace("Kickoff for a new remote control that sells for 25 Euro, with production cost capped at 12.50 Euro and a clear plan for the next meeting.", "Kickoff."), "Summary has 1 words"),
            (GOOD.replace("- One remote for every device", "- [ai] One remote for every device"), "copied note/transcript markup"),
            (GOOD.replace("- Cost capped at 12.50 Euro", "- Cost capped at 14 Euro"), "number not in transcript (14)"),
            (GOOD.replace("- **Team**: Read the brief", "- Team reads the brief"), "action not '- **Owner**: task'"),
            (GOOD.replace("- One remote for every device", "One remote for every device"), "non-bullet line outside Summary"),
        ];
        for (text, expected) in cases {
            let errors = check(&text, &src, 300_000);
            assert!(
                errors.iter().any(|e| e.contains(expected)),
                "{expected}: {errors:?}"
            );
        }
        let long_bullet = format!("- {}", vec!["word"; 31].join(" "));
        let errors = check(
            &GOOD.replace("- One remote for every device", &long_bullet),
            &src,
            300_000,
        );
        assert!(
            errors.iter().any(|e| e.contains("bullet over 30 words")),
            "{errors:?}"
        );
        // 220 + 9 words for a one-minute meeting.
        let padded = format!("{GOOD}\n- {}", vec!["more"; 200].join(" "));
        let errors = check(&padded, &src, 60_000);
        assert!(
            errors.iter().any(|e| e.contains("cap for a 1 min meeting")),
            "{errors:?}"
        );
    }

    #[test]
    fn spoken_prices_support_their_figures() {
        let prices = spoken_prices(
            "It costs twelve fifty, or eleven euros sixty five, or nine point nine five. Forty two people.",
        );
        assert!(spoken_price("9.95", &prices));
        assert!(spoken_price("12.50", &prices));
        assert!(spoken_price("€11.65", &prices));
        assert!(!spoken_price("42.10", &prices));
        assert!(!spoken_price("13.50", &prices));
    }

    #[test]
    fn a_stray_none_goes_when_the_section_has_bullets() {
        let text = "## Summary\nS.\n\n## Decisions\n- None\n- Price set\n\n## Action items\n- None\n\n## Pricing\n- x";
        assert_eq!(
            drop_stray_none(text),
            "## Summary\nS.\n\n## Decisions\n- Price set\n\n## Action items\n- None\n\n## Pricing\n- x"
        );
    }

    #[test]
    fn owners_the_meeting_never_names_are_left_out() {
        let transcript =
            vec!["[01:52] Others: Great. Dev, can you also own the offline spec?".to_string()];
        let notes = [UserNote {
            ms: None,
            text: "Priya: pricing draft Fri".into(),
        }];
        let text = "## Action items\n- **Marketing expert**: Write the offline spec\n- **Dev**: Build the sample workspace\n- **Priya**: Pricing draft by Friday\n- **You**: Send the deck";
        assert_eq!(
            drop_unnamed_owners(text, &transcript, &notes),
            "## Action items\n- Write the offline spec\n- **Dev**: Build the sample workspace\n- **Priya**: Pricing draft by Friday\n- **You**: Send the deck"
        );
    }

    #[test]
    fn the_app_allows_a_little_over_the_length_cap() {
        // 17 minutes: cap 373 words, the app's 428
        let words = |n: usize| vec!["word"; n].join(" ");
        let text = |n: usize| {
            format!("## Summary\n{}\n\n## Decisions\n- None\n\n## Action items\n- None\n\n## A\n- a\n\n## B\n- b", words(n))
        };
        let over = |errors: Vec<String>| errors.iter().any(|e| e.contains("cap"));
        let transcript = vec!["[00:00] A: hi".to_string()];
        assert!(over(check(&text(400), &transcript, 17 * 60_000)));
        assert!(!over(check_with_slack(
            &text(400),
            &transcript,
            17 * 60_000,
            LENGTH_SLACK
        )));
        assert!(over(check_with_slack(
            &text(440),
            &transcript,
            17 * 60_000,
            LENGTH_SLACK
        )));
    }

    /// Developer check: the checker against minutes-distill's teacher
    /// notes, which all pass `tidy_data.py check`. `TIDY_VERDICTS` is a JSON
    /// map meeting -> {end_ms, transcript (app format), summary}.
    #[test]
    #[ignore]
    fn the_teacher_notes_pass_the_checker() {
        let path = std::env::var("TIDY_VERDICTS").expect("TIDY_VERDICTS");
        let all: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut rejected = Vec::new();
        for (meeting, v) in &all {
            let transcript = transcript_lines(v["transcript"].as_str().unwrap());
            let errors = check(
                v["summary"].as_str().unwrap().trim(),
                &transcript,
                v["end_ms"].as_i64().unwrap(),
            );
            if !errors.is_empty() {
                rejected.push(format!("{meeting}: {errors:?}"));
            }
        }
        assert!(
            rejected.is_empty(),
            "{} of {} rejected:\n{}",
            rejected.len(),
            all.len(),
            rejected.join("\n")
        );
    }

    /// Developer check: the tidy adapter on the real helper, on a full
    /// meeting's input. `TIDY_LONG_INPUT` is a JSON {prompt, transcript
    /// (lines), end_ms} from minutes-distill's `tidy_data.build_input`;
    /// needs the E2B model downloaded, `MEETILY_TIDY_LORA` and
    /// `MEETILY_LLAMA_HELPER=target/release/llama-helper`.
    #[tokio::test]
    #[ignore]
    async fn the_adapter_writes_checked_notes_for_a_whole_meeting() {
        #[derive(Deserialize)]
        struct Long {
            prompt: String,
            transcript: Vec<String>,
            end_ms: i64,
        }
        let long: Long = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("TIDY_LONG_INPUT").expect("TIDY_LONG_INPUT"))
                .unwrap(),
        )
        .unwrap();
        let app_data_dir = dirs::home_dir()
            .unwrap()
            .join("Library/Application Support/com.meetily.ai");
        let model = BuiltinTidyModel {
            adapter: available(&app_data_dir).expect("E2B and the tidy adapter"),
            app_data_dir,
            own_helper: true,
        };
        let started = std::time::Instant::now();
        let (answer, truncated) = model
            .generate(SYSTEM_PROMPT, &long.prompt, &CancellationToken::new())
            .await
            .unwrap();
        let answer = drop_stray_none(answer.trim());
        println!("{answer}\n-- {:.1}s", started.elapsed().as_secs_f64());
        assert!(!truncated);
        let errors = check_with_slack(&answer, &long.transcript, long.end_ms, LENGTH_SLACK);
        assert!(errors.is_empty(), "{errors:?}");
    }

    struct Answer(
        Result<(String, bool), String>,
        std::sync::Mutex<Vec<String>>,
    );

    #[async_trait::async_trait]
    impl TidyModel for Answer {
        async fn generate(
            &self,
            system: &str,
            user: &str,
            _: &CancellationToken,
        ) -> Result<(String, bool), String> {
            assert_eq!(system, SYSTEM_PROMPT);
            self.1.lock().unwrap().push(user.to_string());
            self.0.clone()
        }
    }

    async fn run_with(answer: Result<(String, bool), String>) -> (Option<TidyNotes>, Vec<String>) {
        let notes = [
            live(0, "point", "Price at 25 Euro", None, "Pricing"),
            live(0, "point", "Cost twelve fifty", None, "Pricing"),
        ];
        let runs = crate::summary::live_outline::topic_runs(&notes, 300_000);
        let model = Answer(answer, Default::default());
        let result = run(
            &model,
            TidyInputs {
                runs: &runs,
                user_notes: &[],
                transcript_text: "[00:10] [Others] It will sell for twenty five Euro.\n[00:20] [Others] Production cost is twelve fifty.",
                end_ms: 300_000,
            },
            &CancellationToken::new(),
        )
        .await;
        let prompts = model.1.lock().unwrap().clone();
        (result, prompts)
    }

    #[tokio::test]
    async fn the_pass_keeps_a_checked_answer_and_drops_the_rest() {
        let (kept, prompts) = run_with(Ok((format!("\n{GOOD}\n"), false))).await;
        // David is never named in this transcript, so his name goes.
        assert_eq!(kept.unwrap().markdown, GOOD.replace("**David**: ", ""));
        assert!(prompts[0].starts_with("MEETING LENGTH: 5 min\nLIVE NOTES:\n## Pricing (from 00:00)\n- [ai] Price at 25 Euro\n"));
        assert!(prompts[0].ends_with("TRANSCRIPT:\n[00:10] Others: It will sell for twenty five Euro.\n[00:20] Others: Production cost is twelve fifty."));
        assert!(
            run_with(Ok((GOOD.to_string(), true))).await.0.is_none(),
            "cut off"
        );
        assert!(
            run_with(Ok(("## Summary\nToo short".into(), false)))
                .await
                .0
                .is_none(),
            "rejected"
        );
        assert!(
            run_with(Err("helper died".into())).await.0.is_none(),
            "failed"
        );
    }
}
