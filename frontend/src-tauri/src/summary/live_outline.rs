//! Enhanced notes written from the live notes, one topic at a time.
//!
//! While a meeting records, `live_summary` takes short notes under topics
//! (`meeting_timeline_entries`). Each consecutive stretch of notes under one
//! topic is a [`TopicRun`]. When a run closes (the notetaker starts another
//! topic), its Enhanced section is written in the background from the run's
//! notes, its slice of the transcript and the user's own notes typed during
//! it (`live_summary::enhancer`). At stop only the last run is left to write,
//! and the rest of the notes are put together here without a model:
//!
//! - one `## ` section per topic, in order; a topic the meeting came back to
//!   is one section; a topic with at most [`SMALL_TOPIC_NOTES`] notes is not
//!   written by the model but folded into a final `## Also discussed`;
//! - `## Decisions` from the decision notes;
//! - `## Action items` from the action notes, `Owner — task`, kept only when
//!   a transcript line at the note's time shows someone taking it on;
//! - `## Open questions` from what the section calls reported, if any.
//!
//! Every bullet ends with a `[mm:ss]` link to the transcript.
//!
//! Everything here is pure; the model calls are in `live_summary::enhancer`
//! (during the meeting) and `summary::live_enhanced` (at stop and on
//! Re-enhance). Ported from the earlier whole-meeting outline attempt (#104):
//! the transcript and notes parsing, links, follow-up evidence and fallbacks
//! are that branch's.

use std::collections::HashSet;
use std::fmt::Write as _;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::database::repositories::timeline::TimelineEntryRow;
use crate::live_summary::notes::{strip_narration, Narration};
use crate::live_summary::parse::is_repeat;

/// Fewer live notes than one per this many minutes means the old path: the
/// live summary was mostly off or failing.
pub const MINUTES_PER_NOTE_LIMIT: i64 = 5;
/// A topic run with at most this many notes is not written by the model; its
/// notes go under "Also discussed".
pub const SMALL_TOPIC_NOTES: usize = 2;
/// The model's bullets per section (the user's own notes come on top).
pub const MAX_SECTION_BULLETS: usize = 4;
/// Open questions one section may report.
const MAX_SECTION_QUESTIONS: usize = 2;
/// The section call's answer stops after this many lines.
pub const SECTION_MAX_LINES: usize = MAX_SECTION_BULLETS + MAX_SECTION_QUESTIONS + 2;
/// The transcript one section call carries at most (about 4.5k tokens), so a
/// section fits the live notes model's 8k cached context with room to answer.
pub const MAX_SECTION_TRANSCRIPT_CHARS: usize = 18_000;
/// The section holding small topics.
pub const ALSO_DISCUSSED: &str = "Also discussed";
/// A link this far outside its section's time range is dropped.
const LINK_SLACK_MS: i64 = 60_000;
/// How far around an action note its evidence is looked for.
const EVIDENCE_SLACK_MS: i64 = 15_000;

static CLOCK_PREFIX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\[(\d{1,3}:\d{2}(?::\d{2})?)\]").unwrap());
/// `[12:34]`, `[1:02:03]`, `(12:34)`, and ranges `[12:34-13:00]`.
static LINK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\[(](\d{1,3}:\d{2}(?::\d{2})?)(?:\s*[-–—]\s*\d{1,3}:\d{2}(?::\d{2})?)?[\])]")
        .unwrap()
});
static FOLLOW_UP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:[-*•]\s+)?\**(action(?:\s+item)?|decision|question|open question)s?\**\s*:\**\s*(.+)$")
        .unwrap()
});
static BULLET: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\s*)(?:[-*•+]|\d+[.)])\s+(.*)$").unwrap());
/// The speaker label of a summary transcript line: `[mm:ss] [You] text`.
static SPEAKER: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\[[\d:]+\]\s+\[([^\]]+)\]").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Point,
    Decision,
    Action,
}

impl NoteKind {
    fn from_stored(kind: &str) -> Self {
        match kind {
            "decision" => Self::Decision,
            "action" => Self::Action,
            _ => Self::Point,
        }
    }
}

/// One live note, as the enhanced notes use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveNote {
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: NoteKind,
    pub text: String,
    /// An action's owner; None for points and decisions.
    pub speaker: Option<String>,
    pub topic: Option<String>,
}

impl LiveNote {
    pub fn new(
        start_ms: i64,
        end_ms: i64,
        kind: &str,
        text: &str,
        speaker: Option<&str>,
        topic: Option<&str>,
    ) -> Self {
        let clean = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Self {
            start_ms,
            end_ms: end_ms.max(start_ms),
            kind: NoteKind::from_stored(kind),
            text: text.trim().to_string(),
            speaker: clean(speaker),
            topic: clean(topic),
        }
    }
}

impl From<&TimelineEntryRow> for LiveNote {
    fn from(row: &TimelineEntryRow) -> Self {
        Self::new(
            row.start_ms,
            row.end_ms,
            &row.kind,
            &row.text,
            row.speaker.as_deref(),
            row.topic.as_deref(),
        )
    }
}

/// One line of the summary transcript (`[mm:ss] [Speaker] text`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    /// Audio time; a line without a timestamp takes the previous line's.
    pub ms: i64,
    pub text: String,
}

impl TranscriptLine {
    /// The `[You]` / `[Others]` / `[Name]` label, if the line has one.
    pub fn speaker(&self) -> Option<&str> {
        SPEAKER
            .captures(&self.text)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
    }
}

/// One of the user's own notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserNote {
    /// When the user typed it (audio time), when known.
    pub ms: Option<i64>,
    pub text: String,
}

/// Why a meeting's notes are written the old way (the whole transcript).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    NoNotes,
    /// Notes made before live notes had topics.
    NoTopics,
    /// The transcript lines carry no `[mm:ss]` to slice by.
    NoTimestamps,
    Sparse {
        notes: usize,
        minutes: i64,
    },
}

impl std::fmt::Display for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoNotes => f.write_str("the meeting has no live notes"),
            Self::NoTopics => f.write_str("the live notes have no topics"),
            Self::NoTimestamps => f.write_str("the transcript has no timestamps"),
            Self::Sparse { notes, minutes } => write!(
                f,
                "{notes} live notes for {minutes} minutes is fewer than one per {MINUTES_PER_NOTE_LIMIT} minutes"
            ),
        }
    }
}

/// `mm:ss`, with minutes past 59 (as the summary transcript writes them).
pub fn format_transcript_clock(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// `mm:ss` or `h:mm:ss` to milliseconds; seconds must be below 60.
pub fn parse_transcript_clock(text: &str) -> Option<i64> {
    let parts: Vec<i64> = text
        .split(':')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    let seconds = match parts.as_slice() {
        [m, s] if *s < 60 => m * 60 + s,
        [h, m, s] if *s < 60 && *m < 60 => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(seconds * 1000)
}

/// The summary transcript's lines, and how many carried a timestamp.
pub fn parse_transcript(text: &str) -> (Vec<TranscriptLine>, usize) {
    let mut lines = Vec::new();
    let mut timed = 0;
    let mut last = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let ms = CLOCK_PREFIX
            .captures(line)
            .and_then(|c| parse_transcript_clock(&c[1]));
        if let Some(ms) = ms {
            timed += 1;
            last = ms;
        }
        lines.push(TranscriptLine {
            ms: ms.unwrap_or(last),
            text: line.to_string(),
        });
    }
    (lines, timed)
}

/// A transcript line in the summary format, from a live segment.
pub fn transcript_line(start_ms: i64, speaker: &str, text: &str) -> TranscriptLine {
    TranscriptLine {
        ms: start_ms,
        text: format!(
            "[{}] [{speaker}] {}",
            format_transcript_clock(start_ms),
            text.trim()
        ),
    }
}

/// Whether the meeting's live notes can carry its enhanced notes, or why not.
pub fn check(notes: &[LiveNote], lines: &[TranscriptLine], timed: usize) -> Result<(), Fallback> {
    let notes: Vec<&LiveNote> = notes.iter().filter(|n| !n.text.is_empty()).collect();
    if notes.is_empty() {
        return Err(Fallback::NoNotes);
    }
    if notes.iter().all(|n| n.topic.is_none()) {
        return Err(Fallback::NoTopics);
    }
    if timed == 0 {
        return Err(Fallback::NoTimestamps);
    }
    let duration = lines
        .iter()
        .map(|l| l.ms)
        .chain(notes.iter().map(|n| n.end_ms))
        .max()
        .unwrap_or(0);
    let minutes = (duration + 59_999) / 60_000;
    if (notes.len() as i64) * MINUTES_PER_NOTE_LIMIT < minutes {
        return Err(Fallback::Sparse {
            notes: notes.len(),
            minutes,
        });
    }
    Ok(())
}

fn same_title(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// A stretch of consecutive notes under one topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicRun {
    pub title: String,
    /// The transcript the run covers, `[start_ms, end_ms)`: from the previous
    /// run's end (0 for the first) to the next run's first note.
    pub start_ms: i64,
    pub end_ms: i64,
    pub notes: Vec<LiveNote>,
}

impl TopicRun {
    pub fn is_small(&self) -> bool {
        self.notes.len() <= SMALL_TOPIC_NOTES
    }

    pub fn contains(&self, ms: i64) -> bool {
        ms >= self.start_ms && ms < self.end_ms
    }

    pub fn lines<'a>(&self, transcript: &'a [TranscriptLine]) -> Vec<&'a TranscriptLine> {
        transcript.iter().filter(|l| self.contains(l.ms)).collect()
    }
}

/// The meeting's topic runs, in order. A note without a topic sits under the
/// one before it (the first topic, before any). The last run reaches
/// `end_ms` (or its last note, if later).
pub fn topic_runs(notes: &[LiveNote], end_ms: i64) -> Vec<TopicRun> {
    let mut notes: Vec<LiveNote> = notes
        .iter()
        .filter(|n| !n.text.is_empty())
        .cloned()
        .collect();
    notes.sort_by_key(|n| (n.start_ms, n.end_ms));
    let Some(first_topic) = notes.iter().find_map(|n| n.topic.clone()) else {
        return Vec::new();
    };
    let mut current = first_topic;
    let mut runs: Vec<TopicRun> = Vec::new();
    for mut note in notes {
        match &note.topic {
            Some(topic) => current = topic.clone(),
            None => note.topic = Some(current.clone()),
        }
        match runs.last_mut() {
            Some(run) if same_title(&run.title, &current) => run.notes.push(note),
            _ => runs.push(TopicRun {
                title: current.clone(),
                start_ms: note.start_ms,
                end_ms: note.start_ms,
                notes: vec![note],
            }),
        }
    }
    let count = runs.len();
    for index in 0..count {
        let start = if index == 0 {
            0
        } else {
            runs[index - 1].end_ms
        };
        let end = if index + 1 < count {
            runs[index + 1].notes[0].start_ms
        } else {
            let last_note = runs[index]
                .notes
                .iter()
                .map(|n| n.end_ms)
                .max()
                .unwrap_or(0);
            end_ms.max(last_note) + 1
        };
        runs[index].start_ms = start;
        runs[index].end_ms = end.max(start);
    }
    runs
}

/// The user's notes typed in `[start, end)`.
pub fn user_notes_in(notes: &[UserNote], start_ms: i64, end_ms: i64) -> Vec<&UserNote> {
    notes
        .iter()
        .filter(|n| n.ms.is_some_and(|ms| ms >= start_ms && ms < end_ms))
        .collect()
}

const PLACEMENT_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "are", "was", "were", "has", "have",
    "had", "not", "but", "its", "into", "than", "then", "over", "about", "also", "very", "more",
    "most", "now", "their", "they", "them", "will", "can", "all", "any", "out", "who", "what",
    "our", "your", "you", "his", "her", "she", "been", "being", "others", "yeah", "okay", "like",
    "just", "know", "think", "really", "right", "would", "could", "should", "there", "which",
    "when", "where", "how", "why", "some", "going", "want", "need", "get", "got", "one", "did",
    "does", "let", "say", "said", "see", "way", "thing", "things", "kind", "lot", "yes",
];

fn content_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !PLACEMENT_STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Gives each untimed user note the start of the run it shares the most
/// words with (title ×3, notes ×2, transcript ×1), or of the run of the note
/// before it when it shares none; timed notes keep their time.
pub fn place_user_notes(
    runs: &[TopicRun],
    transcript: &[TranscriptLine],
    notes: Vec<UserNote>,
) -> Vec<UserNote> {
    if runs.is_empty() {
        return notes;
    }
    let profiles: Vec<(HashSet<String>, HashSet<String>, HashSet<String>)> = runs
        .iter()
        .map(|run| {
            let texts = run
                .notes
                .iter()
                .map(|n| n.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let lines = run
                .lines(transcript)
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (
                content_words(&run.title),
                content_words(&texts),
                content_words(&lines),
            )
        })
        .collect();
    let mut previous = 0;
    notes
        .into_iter()
        .map(|mut note| {
            match note.ms {
                Some(ms) => {
                    previous = runs.iter().rposition(|r| r.start_ms <= ms).unwrap_or(0);
                }
                None => {
                    let words = content_words(&note.text);
                    let (best, score) = profiles
                        .iter()
                        .enumerate()
                        .map(|(i, (title, texts, lines))| {
                            let score = 3 * words.intersection(title).count()
                                + 2 * words.intersection(texts).count()
                                + words.intersection(lines).count();
                            (i, score)
                        })
                        .fold((0, 0), |best, cur| if cur.1 > best.1 { cur } else { best });
                    if score > 0 {
                        previous = best;
                    }
                    note.ms = Some(runs[previous].start_ms);
                }
            }
            note
        })
        .collect()
}

fn strip_markers(line: &str) -> &str {
    let line = line.trim().trim_start_matches('#').trim();
    BULLET
        .captures(line)
        .and_then(|c| c.get(2))
        .map_or(line, |m| m.as_str())
        .trim()
}

/// Free-form notes (Markdown) as one note per line, without times; indented
/// lines stay with the note above.
pub fn parse_user_notes(markdown: &str) -> Vec<UserNote> {
    static TIMED: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\[(\d{1,3}:\d{2}(?::\d{2})?)\](?:\s+\**important:?\**)?\s*(.*)$").unwrap()
    });
    let mut out: Vec<UserNote> = Vec::new();
    for raw in markdown.lines() {
        if raw.trim().is_empty()
            || raw
                .trim()
                .chars()
                .all(|c| matches!(c, '-' | '*' | '_' | '='))
        {
            continue;
        }
        let indented = raw.starts_with("  ") || raw.starts_with('\t');
        let text = strip_markers(raw);
        if text.is_empty() {
            continue;
        }
        let (ms, text) = match TIMED.captures(text) {
            Some(c) => (parse_transcript_clock(&c[1]), c[2].trim().to_string()),
            None => (None, text.to_string()),
        };
        if text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if indented => {
                let _ = write!(last.text, "\n  - {text}");
            }
            _ => out.push(UserNote { ms, text }),
        }
    }
    out
}

/// System prompt of one section call.
pub fn section_system_prompt() -> &'static str {
    r#"You write one section of a meeting's notes: the part of the meeting named in the request.

Transcript lines look like `[mm:ss] [Speaker] text`. `[You]` is the person recording; `[Others]` is everyone else, possibly several people.

Write at most 4 bullets (`- `), the most important first:
- Facts first: names, numbers, dates, what was decided. One fact per bullet, at most 20 words.
- Start from the live notes: correct and complete them from the transcript. Never repeat a point or the user's own notes.
- End every bullet with the [mm:ss] of the transcript line it comes from.
- Say what was said, not who said it. Never write "You", "Others", "they", "the speaker" or a person's name as the one who said or did something, unless that line itself names them doing it.
- Only what the transcript says. No heading, no introduction.

If a question was asked and left unanswered, add after the bullets:
Question: the question [mm:ss]

Example (format only):
- Office move booked for 3 June, budget capped at $4,000 [02:10]
- Window desks still undecided between design and sales [03:05]
Question: Who gets the window desks? [03:05]"#
}

fn note_line(note: &LiveNote) -> String {
    let time = format_transcript_clock(note.start_ms);
    match note.kind {
        NoteKind::Point => format!("[{time}] {}", note.text),
        NoteKind::Decision => format!("[{time}] Decision: {}", note.text),
        NoteKind::Action => format!("[{time}] Action: {}", note.text),
    }
}

/// The transcript lines a section call carries: all of them when they fit
/// `max_chars`, else the lines closest to the run's notes, in time order.
pub fn fit_lines<'a>(
    lines: &[&'a TranscriptLine],
    notes: &[LiveNote],
    max_chars: usize,
) -> Vec<&'a TranscriptLine> {
    let total: usize = lines.iter().map(|l| l.text.len() + 1).sum();
    if total <= max_chars {
        return lines.to_vec();
    }
    let distance = |ms: i64| {
        notes
            .iter()
            .map(|n| {
                if ms < n.start_ms {
                    n.start_ms - ms
                } else if ms > n.end_ms {
                    ms - n.end_ms
                } else {
                    0
                }
            })
            .min()
            .unwrap_or(0)
    };
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by_key(|&i| (distance(lines[i].ms), i));
    let mut keep = vec![false; lines.len()];
    let mut size = 0;
    for i in order {
        let len = lines[i].text.len() + 1;
        if size + len > max_chars {
            continue;
        }
        size += len;
        keep[i] = true;
    }
    lines
        .iter()
        .zip(keep)
        .filter_map(|(line, keep)| keep.then_some(*line))
        .collect()
}

/// User prompt of one section call.
pub fn section_user_prompt(
    run: &TopicRun,
    user_notes: &[&UserNote],
    lines: &[&TranscriptLine],
) -> String {
    let mut prompt = String::new();
    let _ = writeln!(
        prompt,
        "Part of the meeting: {} ({}–{})\n",
        run.title,
        format_transcript_clock(run.start_ms),
        format_transcript_clock(run.end_ms)
    );
    prompt.push_str("Live notes:\n");
    for note in &run.notes {
        prompt.push_str(&note_line(note));
        prompt.push('\n');
    }
    if !user_notes.is_empty() {
        prompt.push_str("\nThe user's own notes (already in the notes; do not repeat):\n");
        for note in user_notes {
            let _ = writeln!(
                prompt,
                "- {}",
                note.text.lines().next().unwrap_or("").trim()
            );
        }
    }
    prompt.push_str("\nTranscript:\n");
    for line in fit_lines(lines, &run.notes, MAX_SECTION_TRANSCRIPT_CHARS) {
        prompt.push_str(&line.text);
        prompt.push('\n');
    }
    prompt.push_str("\nWrite the bullets.");
    prompt
}

/// An action item, decision or open question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUp {
    pub text: String,
    /// The action's owner.
    pub owner: Option<String>,
    pub ms: Option<i64>,
}

/// What one section call produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionReply {
    /// `- text [mm:ss]` lines.
    pub bullets: Vec<String>,
    pub questions: Vec<FollowUp>,
}

impl SectionReply {
    /// The questions as stored: `text [mm:ss]`, one per line.
    pub fn questions_text(&self) -> String {
        self.questions
            .iter()
            .map(|q| with_link(&q.text, q.ms))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Takes the `[mm:ss]` links out of `text`: the text without them, and the
/// first link that falls in `span` (± a minute).
fn take_links(text: &str, span: (i64, i64)) -> (String, Option<i64>) {
    let mut found = None;
    let stripped = LINK.replace_all(text, |captures: &regex::Captures| {
        if found.is_none() {
            found = parse_transcript_clock(&captures[1])
                .filter(|ms| *ms >= span.0 - LINK_SLACK_MS && *ms <= span.1 + LINK_SLACK_MS);
        }
        String::new()
    });
    let cleaned = stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches([',', ';', '-', '—'])
        .trim()
        .to_string();
    (cleaned, found)
}

/// The transcript line `text` most likely comes from: the most shared
/// content words, at least two.
fn best_line(text: &str, lines: &[&TranscriptLine]) -> Option<i64> {
    let words = content_words(text);
    lines
        .iter()
        .map(|line| {
            (
                content_words(&line.text).intersection(&words).count(),
                line.ms,
            )
        })
        .filter(|(shared, _)| *shared >= 2)
        .fold(None, |best: Option<(usize, i64)>, cur| match best {
            Some(b) if b.0 >= cur.0 => Some(b),
            _ => Some(cur),
        })
        .map(|(_, ms)| ms)
}

/// The line a link points at: the transcript line at or just before `ms`
/// (models round), so a link always lands on a line that exists.
fn snap_to_line(ms: i64, lines: &[&TranscriptLine]) -> i64 {
    lines
        .iter()
        .map(|l| l.ms)
        .filter(|line_ms| *line_ms <= ms + 1_000)
        .max()
        .or_else(|| lines.first().map(|l| l.ms))
        .unwrap_or(ms)
}

pub fn with_link(text: &str, ms: Option<i64>) -> String {
    match ms {
        Some(ms) => format!("{text} [{}]", format_transcript_clock(ms)),
        None => text.to_string(),
    }
}

/// A bullet as the notes keep it: narration of who spoke taken out (or the
/// bullet dropped when nothing is left).
fn clean_bullet(text: &str) -> Option<String> {
    let text = text.trim().trim_matches('*').trim();
    match strip_narration(text) {
        Narration::Clean => Some(text.to_string()),
        Narration::Rewritten(content) => Some(content),
        Narration::Drop => None,
    }
    .filter(|t| t.split_whitespace().count() >= 3)
}

/// Reads a section call's reply: at most [`MAX_SECTION_BULLETS`] bullets
/// (links checked against the run's span and snapped to a transcript line; a
/// missing one added from the best-matching line; narration removed; repeats
/// of each other or of the user's notes dropped) and `Question:` lines.
/// Headings, introductions and other follow-up lines are dropped.
pub fn parse_section_reply(
    raw: &str,
    span: (i64, i64),
    lines: &[&TranscriptLine],
    user_notes: &[&UserNote],
) -> SectionReply {
    let mut reply = SectionReply::default();
    let mut seen: Vec<String> = user_notes.iter().map(|n| n.text.clone()).collect();
    for raw_line in raw.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("```") {
            continue;
        }
        if let Some(captures) = FOLLOW_UP.captures(trimmed) {
            let label = captures[1].to_lowercase();
            if !label.contains("question") || reply.questions.len() >= MAX_SECTION_QUESTIONS {
                continue;
            }
            let (text, ms) = take_links(captures[2].trim().trim_matches('*').trim(), span);
            if text.split_whitespace().count() < 3 {
                continue;
            }
            let ms = ms.or_else(|| best_line(&text, lines));
            reply.questions.push(FollowUp {
                text,
                owner: None,
                ms: ms.map(|ms| snap_to_line(ms, lines)),
            });
            continue;
        }
        let body = match BULLET.captures(raw_line) {
            // Sub-bullets: the section stays flat and short.
            Some(c) if c[1].len() >= 2 => continue,
            Some(c) => c[2].trim().to_string(),
            None if trimmed.ends_with(':') => continue,
            None => trimmed.to_string(),
        };
        if reply.bullets.len() >= MAX_SECTION_BULLETS {
            continue;
        }
        let (text, ms) = take_links(&body, span);
        let Some(text) = clean_bullet(&text) else {
            continue;
        };
        if seen.iter().any(|other| is_repeat(&text, other)) {
            continue;
        }
        // No link of its own and no matching line: the run's first line.
        let ms = ms
            .or_else(|| best_line(&text, lines))
            .or_else(|| lines.first().map(|l| l.ms))
            .map(|ms| snap_to_line(ms, lines));
        seen.push(text.clone());
        reply.bullets.push(format!("- {}", with_link(&text, ms)));
    }
    reply
}

/// A run's bullets without a model: its notes as written.
pub fn fallback_reply(run: &TopicRun) -> SectionReply {
    SectionReply {
        bullets: run
            .notes
            .iter()
            .filter(|n| n.kind == NoteKind::Point)
            .take(MAX_SECTION_BULLETS)
            .map(|n| format!("- {}", with_link(&n.text, Some(n.start_ms))))
            .collect(),
        ..Default::default()
    }
}

/// A section's body as stored: the user's notes typed during it, word for
/// word, then the model's bullets.
pub fn section_body(user_notes: &[&UserNote], reply: &SectionReply) -> String {
    let mut lines: Vec<String> = Vec::new();
    for note in user_notes {
        let mut parts = note.text.lines();
        let head = parts.next().unwrap_or("").trim();
        if head.is_empty() {
            continue;
        }
        lines.push(format!("- {head}"));
        lines.extend(parts.map(str::to_string));
    }
    lines.extend(reply.bullets.iter().cloned());
    lines.join("\n")
}

/// Someone taking a task on, in a transcript line: first person only
/// ("you'll" / "let's" lines were mostly a suggestion read as a task).
static COMMITMENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(i'll|i will|i can (?:send|share|introduce|intro|connect|help|set up|make|do|put|get|follow)|i'm going to|i am going to|let me|we'll|we will|happy to|i'd be happy|i would be happy)\b",
    )
    .unwrap()
});
static NEGATED_COMMITMENT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(i'll|i will|we'll|we will)\s+(not|never)\b").unwrap());

/// The transcript line that backs an action note: one within the note's time
/// (± a little) where someone takes something on. The nearest to the note's
/// start wins.
pub fn action_evidence<'a>(
    note: &LiveNote,
    lines: &[&'a TranscriptLine],
) -> Option<&'a TranscriptLine> {
    let from = note.start_ms - EVIDENCE_SLACK_MS;
    let until = note.end_ms.max(note.start_ms) + EVIDENCE_SLACK_MS;
    lines
        .iter()
        .filter(|l| l.ms >= from && l.ms <= until)
        .filter(|l| COMMITMENT.is_match(&l.text) && !NEGATED_COMMITMENT.is_match(&l.text))
        .min_by_key(|l| ((l.ms - note.start_ms).abs(), l.ms))
        .copied()
}

/// The owner of an action, from who said the committing line: `You` for a
/// `[You]` line, else the note's named owner when that line names them,
/// else `Others`. Never a name the transcript does not show.
fn evidenced_owner(note: &LiveNote, evidence: &TranscriptLine) -> String {
    match evidence.speaker() {
        Some("You") => "You".to_string(),
        speaker => {
            let named = note
                .speaker
                .as_deref()
                .filter(|o| !matches!(*o, "You" | "Others"))
                .filter(|name| {
                    let lower = evidence.text.to_lowercase();
                    name.split_whitespace()
                        .all(|part| lower.contains(&part.to_lowercase()))
                });
            match (named, speaker) {
                (Some(name), _) => name.to_string(),
                (None, Some(label)) if label != "Others" && !label.is_empty() => label.to_string(),
                _ => "Others".to_string(),
            }
        }
    }
}

/// The action items: action notes backed by a committing transcript line,
/// owner from that line, linked to it; repeats dropped.
pub fn action_items(notes: &[LiveNote], transcript: &[TranscriptLine]) -> Vec<FollowUp> {
    let lines: Vec<&TranscriptLine> = transcript.iter().collect();
    let mut items: Vec<FollowUp> = Vec::new();
    for note in notes.iter().filter(|n| n.kind == NoteKind::Action) {
        let Some(evidence) = action_evidence(note, &lines) else {
            continue;
        };
        let item = FollowUp {
            text: note.text.clone(),
            owner: Some(evidenced_owner(note, evidence)),
            ms: Some(evidence.ms),
        };
        if !items.iter().any(|other| is_repeat(&item.text, &other.text)) {
            items.push(item);
        }
    }
    items
}

/// The decisions: the decision notes, repeats dropped.
pub fn decisions(notes: &[LiveNote]) -> Vec<FollowUp> {
    let mut items: Vec<FollowUp> = Vec::new();
    for note in notes.iter().filter(|n| n.kind == NoteKind::Decision) {
        if !items.iter().any(|other| is_repeat(&note.text, &other.text)) {
            items.push(FollowUp {
                text: note.text.clone(),
                owner: None,
                ms: Some(note.start_ms),
            });
        }
    }
    items
}

/// A written section, as stored (`meeting_enhanced_sections`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenSection {
    pub start_ms: i64,
    pub title: String,
    pub body: String,
    /// `question [mm:ss]` per line.
    pub questions: String,
}

/// One heading of the notes as they are put together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledSection {
    pub title: String,
    pub body: String,
}

/// The topic sections, in order of first appearance: written runs (a topic
/// the meeting returned to is one section), a small run under a topic that
/// has a written run adds its notes there, and a written run missing from
/// `written` (not written yet) is skipped. Small topics come back separately,
/// for "Also discussed".
pub fn topic_sections(
    runs: &[TopicRun],
    written: &[WrittenSection],
) -> (Vec<AssembledSection>, Vec<String>) {
    let mut sections: Vec<AssembledSection> = Vec::new();
    let mut also: Vec<String> = Vec::new();
    let written_for = |run: &TopicRun| written.iter().find(|w| w.start_ms == run.start_ms);
    let has_written = |title: &str| {
        runs.iter()
            .any(|r| same_title(&r.title, title) && written_for(r).is_some())
    };
    for run in runs {
        let body = match written_for(run) {
            Some(section) => section.body.clone(),
            None if run.is_small() && has_written(&run.title) => run
                .notes
                .iter()
                .filter(|n| n.kind == NoteKind::Point)
                .map(|n| format!("- {}", with_link(&n.text, Some(n.start_ms))))
                .collect::<Vec<_>>()
                .join("\n"),
            None if run.is_small() => {
                also.extend(
                    run.notes
                        .iter()
                        .filter(|n| n.kind == NoteKind::Point)
                        .map(|n| format!("- {}", with_link(&n.text, Some(n.start_ms)))),
                );
                continue;
            }
            None => continue,
        };
        if body.trim().is_empty() {
            continue;
        }
        match sections
            .iter_mut()
            .find(|s| same_title(&s.title, &run.title))
        {
            Some(section) => {
                section.body.push('\n');
                section.body.push_str(&body);
            }
            None => sections.push(AssembledSection {
                title: run.title.clone(),
                body,
            }),
        }
    }
    (sections, also)
}

fn owner_label(owner: Option<&str>) -> String {
    owner
        .map(|o| o.trim().trim_matches('*').trim())
        .filter(|o| !o.is_empty())
        .unwrap_or("Others")
        .to_string()
}

/// "Also discussed", "Decisions", "Action items" and "Open questions", as
/// Markdown (empty ones left out).
pub fn tail_markdown(
    also: &[String],
    decisions: &[FollowUp],
    actions: &[FollowUp],
    questions: &[String],
) -> String {
    let mut out = String::new();
    if !also.is_empty() {
        let _ = write!(out, "## {ALSO_DISCUSSED}\n\n{}\n\n", also.join("\n"));
    }
    if !decisions.is_empty() {
        out.push_str("## Decisions\n\n");
        for item in decisions {
            let _ = writeln!(out, "- {}", with_link(&item.text, item.ms));
        }
        out.push('\n');
    }
    if !actions.is_empty() {
        out.push_str("## Action items\n\n");
        for item in actions {
            let _ = writeln!(
                out,
                "- {} — {}",
                owner_label(item.owner.as_deref()),
                with_link(&item.text, item.ms)
            );
        }
        out.push('\n');
    }
    if !questions.is_empty() {
        let _ = write!(out, "## Open questions\n\n{}\n\n", questions.join("\n"));
    }
    out.trim_end().to_string()
}

/// The open questions the sections reported, repeats dropped.
pub fn open_questions(written: &[WrittenSection]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in written
        .iter()
        .flat_map(|s| s.questions.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let bare = LINK.replace_all(line, "").to_string();
        if !out
            .iter()
            .any(|other| is_repeat(&bare, &LINK.replace_all(other, "")))
        {
            out.push(format!("- {line}"));
        }
    }
    out
}

/// The finished notes: the title, the topics, then the tail.
pub fn assemble(
    title: &str,
    runs: &[TopicRun],
    written: &[WrittenSection],
    notes: &[LiveNote],
    transcript: &[TranscriptLine],
) -> String {
    let (sections, also) = topic_sections(runs, written);
    let mut out = format!("# {}\n", title.trim());
    for section in &sections {
        let _ = write!(
            out,
            "\n## {}\n\n{}\n",
            section.title,
            section.body.trim_end()
        );
    }
    let tail = tail_markdown(
        &also,
        &decisions(notes),
        &action_items(notes, transcript),
        &open_questions(written),
    );
    if !tail.is_empty() {
        out.push('\n');
        out.push_str(&tail);
        out.push('\n');
    }
    out
}

/// One heading of the Enhanced notes while the meeting records.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LiveSection {
    pub title: String,
    /// What is written so far (Markdown bullets); empty while writing.
    pub body: String,
    /// Still being written: the topic under way, or a closed one whose
    /// section has not come back yet.
    pub writing: bool,
}

/// The Enhanced notes as they stand during the meeting: the written topics,
/// the closed topics still being written and the topic under way (the last
/// run), then the tail (small topics, decisions, actions, questions so far).
pub fn live_view(
    runs: &[TopicRun],
    written: &[WrittenSection],
    notes: &[LiveNote],
    transcript: &[TranscriptLine],
) -> (Vec<LiveSection>, String) {
    let mut sections: Vec<LiveSection> = Vec::new();
    let mut also: Vec<String> = Vec::new();
    let has_written = |title: &str| {
        runs.iter().any(|r| {
            same_title(&r.title, title) && written.iter().any(|w| w.start_ms == r.start_ms)
        })
    };
    let last = runs.len().saturating_sub(1);
    for (index, run) in runs.iter().enumerate() {
        let points = || {
            run.notes
                .iter()
                .filter(|n| n.kind == NoteKind::Point)
                .map(|n| format!("- {}", with_link(&n.text, Some(n.start_ms))))
                .collect::<Vec<_>>()
        };
        let (body, writing) = match written.iter().find(|w| w.start_ms == run.start_ms) {
            Some(section) => (section.body.clone(), false),
            None if index == last => (String::new(), true),
            None if run.is_small() && has_written(&run.title) => (points().join("\n"), false),
            None if run.is_small() => {
                also.extend(points());
                continue;
            }
            None => (String::new(), true),
        };
        match sections
            .iter_mut()
            .find(|s| same_title(&s.title, &run.title))
        {
            Some(section) => {
                if !body.is_empty() {
                    if !section.body.is_empty() {
                        section.body.push('\n');
                    }
                    section.body.push_str(&body);
                }
                section.writing |= writing;
            }
            None => sections.push(LiveSection {
                title: run.title.clone(),
                body,
                writing,
            }),
        }
    }
    let tail = tail_markdown(
        &also,
        &decisions(notes),
        &action_items(notes, transcript),
        &open_questions(written),
    );
    (sections, tail)
}

/// The notes as a short outline for the title call: topics and their notes.
pub fn title_source(runs: &[TopicRun]) -> String {
    let mut out = String::new();
    for run in runs {
        let _ = writeln!(out, "{}:", run.title);
        for note in &run.notes {
            let _ = writeln!(out, "- {}", note.text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(min: f64, kind: &str, text: &str, topic: Option<&str>) -> LiveNote {
        let start = (min * 60_000.0) as i64;
        let speaker = (kind == "action").then_some("Others");
        LiveNote::new(start, start + 20_000, kind, text, speaker, topic)
    }

    fn transcript(minutes: i64) -> Vec<TranscriptLine> {
        (0..minutes * 2)
            .map(|half| transcript_line(half * 30_000, "Others", &format!("talking point {half}")))
            .collect()
    }

    #[test]
    fn clocks_round_trip_in_the_transcript_format() {
        assert_eq!(format_transcript_clock(75 * 60_000 + 12_000), "75:12");
        assert_eq!(parse_transcript_clock("75:12"), Some(75 * 60_000 + 12_000));
        assert_eq!(parse_transcript_clock("1:02:03"), Some(3_723_000));
        assert_eq!(parse_transcript_clock("12:75"), None);
    }

    #[test]
    fn transcript_lines_take_their_time_or_the_previous_one() {
        let (lines, timed) =
            parse_transcript("[00:05] [You] hi\nno time here\n[01:10] [Others] yo");
        assert_eq!(timed, 2);
        assert_eq!(
            lines.iter().map(|l| l.ms).collect::<Vec<_>>(),
            [5_000, 5_000, 70_000]
        );
        assert_eq!(lines[0].speaker(), Some("You"));
        assert_eq!(lines[1].speaker(), None);
        assert_eq!(
            transcript_line(61_000, "You", " ok ").text,
            "[01:01] [You] ok"
        );
    }

    #[test]
    fn runs_follow_the_topics_and_cover_the_meeting_without_gaps() {
        let notes = vec![
            note(0.5, "point", "Intro of the team", Some("Team Intro")),
            note(2.0, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(3.0, "point", "No topic, so the one before", None),
            note(4.0, "decision", "Ship annual plan first", Some("pricing")),
            note(6.0, "point", "Hiring two engineers", Some("Hiring")),
            note(9.0, "point", "Back to pricing", Some("Pricing")),
        ];
        let runs = topic_runs(&notes, 600_000);
        let spans: Vec<(&str, i64, i64, usize)> = runs
            .iter()
            .map(|r| (r.title.as_str(), r.start_ms, r.end_ms, r.notes.len()))
            .collect();
        assert_eq!(
            spans,
            [
                ("Team Intro", 0, 120_000, 1),
                ("Pricing", 120_000, 360_000, 3),
                ("Hiring", 360_000, 540_000, 1),
                ("Pricing", 540_000, 600_001, 1),
            ]
        );
        assert!(runs[0].is_small() && !runs[1].is_small());
    }

    #[test]
    fn falls_back_without_notes_topics_timestamps_or_density() {
        let lines = transcript(30);
        assert_eq!(check(&[], &lines, 60), Err(Fallback::NoNotes));
        let untopiced = vec![note(1.0, "point", "a note here", None)];
        assert_eq!(check(&untopiced, &lines, 60), Err(Fallback::NoTopics));
        let one = vec![note(1.0, "point", "a note here", Some("A"))];
        assert_eq!(check(&one, &lines, 0), Err(Fallback::NoTimestamps));
        assert_eq!(
            check(&one, &lines, 60),
            Err(Fallback::Sparse {
                notes: 1,
                minutes: 30
            })
        );
        let dense: Vec<LiveNote> = (0..6)
            .map(|i| note(i as f64 * 5.0, "point", "a note here", Some("A")))
            .collect();
        assert_eq!(check(&dense, &lines, 60), Ok(()));
    }

    #[test]
    fn user_notes_parse_and_untimed_ones_find_their_topic() {
        let parsed = parse_user_notes(
            "# Prep\n- [01:30] **Important:** follow up on pricing\n- hiring engineers budget?\n  - ask Sam\n* what about it",
        );
        assert_eq!(
            parsed,
            vec![
                UserNote {
                    ms: None,
                    text: "Prep".into()
                },
                UserNote {
                    ms: Some(90_000),
                    text: "follow up on pricing".into()
                },
                UserNote {
                    ms: None,
                    text: "hiring engineers budget?\n  - ask Sam".into()
                },
                UserNote {
                    ms: None,
                    text: "what about it".into()
                },
            ]
        );
        let notes = vec![
            note(0.5, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(3.0, "point", "Two engineers to hire", Some("Hiring")),
        ];
        let runs = topic_runs(&notes, 360_000);
        let placed = place_user_notes(&runs, &transcript(6), parsed);
        // "Prep" shares nothing: the first run; the timed note keeps its time;
        // "hiring engineers" goes to Hiring; the last follows it.
        let times: Vec<Option<i64>> = placed.iter().map(|n| n.ms).collect();
        assert_eq!(times, [Some(0), Some(90_000), Some(180_000), Some(180_000)]);
        assert_eq!(user_notes_in(&placed, 180_000, 360_000).len(), 2);
    }

    #[test]
    fn section_prompt_carries_the_runs_notes_user_notes_and_only_its_transcript() {
        let notes = vec![
            note(0.5, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(1.0, "action", "send the price sheet", Some("Pricing")),
            note(1.5, "point", "Annual discount offered", Some("Pricing")),
            note(3.0, "point", "Two engineers to hire", Some("Hiring")),
        ];
        let lines = transcript(4);
        let runs = topic_runs(&notes, 240_000);
        let user = UserNote {
            ms: Some(50_000),
            text: "annual discount?".into(),
        };
        let prompt = section_user_prompt(&runs[0], &[&user], &runs[0].lines(&lines));
        assert!(prompt.starts_with("Part of the meeting: Pricing (00:00–03:00)"));
        assert!(prompt.contains("[00:30] Pricing at $40 per seat"));
        assert!(prompt.contains("[01:00] Action: send the price sheet"));
        assert!(prompt.contains("already in the notes; do not repeat):\n- annual discount?"));
        assert!(prompt.contains("[02:30] [Others] talking point 5"));
        assert!(!prompt.contains("[03:00] [Others]"));
        assert!(!prompt.contains("Two engineers"));
        assert!(section_system_prompt().contains("[mm:ss]"));
    }

    #[test]
    fn a_long_slice_keeps_the_lines_nearest_the_notes() {
        let lines: Vec<TranscriptLine> = (0..100)
            .map(|i| transcript_line(i * 10_000, "Others", &"x".repeat(90)))
            .collect();
        let refs: Vec<&TranscriptLine> = lines.iter().collect();
        let notes = vec![note(8.0, "point", "a note", Some("A"))];
        let kept = fit_lines(&refs, &notes, 1_100);
        assert!(kept.len() >= 9 && kept.len() <= 11);
        assert!(kept.windows(2).all(|w| w[0].ms < w[1].ms));
        assert!(kept.iter().all(|l| (l.ms - 480_000).abs() <= 60_000));
        assert_eq!(fit_lines(&refs[..3], &notes, 1_100).len(), 3);
    }

    #[test]
    fn replies_keep_four_clean_linked_bullets_and_questions() {
        let (lines, _) = parse_transcript(
            "[01:00] [Others] the annual plan costs forty dollars per seat\n[02:00] [You] hiring two engineers soon\n[02:50] [Others] who owns the migration",
        );
        let lines: Vec<&TranscriptLine> = lines.iter().collect();
        let user = UserNote {
            ms: None,
            text: "Two engineers hiring soon".into(),
        };
        let raw = "Here are the notes:\n## Pricing\n- Annual plan at $40 per seat [01:01]\n  - Discount for 3 years\n- Seat count unknown (45:00)\n- Others discussed the annual plan pricing in detail [01:00-01:30]\n- Hiring two engineers soon\n- Fifth bullet is one too many here [02:00]\n- Sixth\nAction: You — send the deck [02:00]\nQuestion: Who owns the migration? [02:50]";
        let reply = parse_section_reply(raw, (0, 180_000), &lines, &[&user]);
        assert_eq!(
            reply.bullets,
            [
                "- Annual plan at $40 per seat [01:00]",
                "- Seat count unknown [01:00]",
                "- The annual plan pricing in detail [01:00]",
                "- Fifth bullet is one too many here [02:00]",
            ]
        );
        assert_eq!(reply.questions_text(), "Who owns the migration? [02:50]");
    }

    #[test]
    fn a_bullet_without_a_link_gets_the_best_matching_line() {
        let (lines, _) = parse_transcript(
            "[01:00] [Others] the annual plan costs forty dollars\n[02:00] [You] hiring two engineers soon",
        );
        let lines: Vec<&TranscriptLine> = lines.iter().collect();
        let reply = parse_section_reply("- Hiring two engineers", (0, 180_000), &lines, &[]);
        assert_eq!(reply.bullets, ["- Hiring two engineers [02:00]"]);
    }

    #[test]
    fn actions_need_a_committing_line_and_take_its_speaker_as_owner() {
        let (lines, _) = parse_transcript(
            "[01:00] [Others] can you tell me more about your background\n[03:00] [You] sure, I'll send the deck tonight\n[05:00] [Others] I can introduce you to Priya\n[07:00] [Others] we should think about pricing\n[09:00] [You] I will not sign that",
        );
        let notes = vec![
            note(1.0, "action", "share background", Some("A")),
            note(3.0, "action", "send the deck", Some("A")),
            note(5.0, "action", "introduce Priya", Some("A")),
            note(7.0, "action", "think about pricing", Some("A")),
            note(9.0, "action", "sign the contract", Some("A")),
            note(3.1, "action", "send the deck", Some("A")),
        ];
        let items = action_items(&notes, &lines);
        assert_eq!(
            items,
            [
                FollowUp {
                    text: "send the deck".into(),
                    owner: Some("You".into()),
                    ms: Some(180_000)
                },
                FollowUp {
                    text: "introduce Priya".into(),
                    owner: Some("Others".into()),
                    ms: Some(300_000)
                },
            ]
        );
    }

    #[test]
    fn notes_assemble_topics_then_also_discussed_decisions_actions_and_questions() {
        let notes = vec![
            note(0.2, "point", "Hello and introductions", Some("Intro")),
            note(1.0, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(1.5, "decision", "Annual plan ships first", Some("Pricing")),
            note(2.0, "point", "Discount for three years", Some("Pricing")),
            note(4.0, "action", "send the deck", Some("Pricing")),
            note(6.0, "point", "Hiring is paused", Some("Hiring")),
            note(8.0, "point", "Seat minimum is ten", Some("Pricing")),
        ];
        let mut lines = transcript(10);
        lines.push(transcript_line(240_000, "You", "I'll send the deck"));
        lines.sort_by_key(|l| l.ms);
        let runs = topic_runs(&notes, 600_000);
        let written = vec![WrittenSection {
            start_ms: runs[1].start_ms,
            title: "Pricing".into(),
            body: "- $40 per seat, annual first [01:00]".into(),
            questions: "Monthly price later? [02:00]".into(),
        }];
        let markdown = assemble("Pricing Call", &runs, &written, &notes, &lines);
        assert_eq!(
            markdown,
            "# Pricing Call\n\n## Pricing\n\n- $40 per seat, annual first [01:00]\n- Seat minimum is ten [08:00]\n\n## Also discussed\n\n- Hello and introductions [00:12]\n- Hiring is paused [06:00]\n\n## Decisions\n\n- Annual plan ships first [01:30]\n\n## Action items\n\n- You — send the deck [04:00]\n\n## Open questions\n\n- Monthly price later? [02:00]\n"
        );
    }

    #[test]
    fn during_the_meeting_closed_topics_fill_in_and_the_current_one_is_writing() {
        let notes = vec![
            note(0.0, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(1.0, "point", "Annual plan first", Some("Pricing")),
            note(2.0, "point", "Three year discount", Some("Pricing")),
            note(3.0, "point", "Hello there everyone", Some("Small Talk")),
            note(4.0, "point", "Two engineers to hire", Some("Hiring")),
            note(5.0, "point", "Backend first", Some("Hiring")),
            note(6.0, "point", "Interviews next week", Some("Hiring")),
            note(7.0, "decision", "Hire remote", Some("Roadmap")),
        ];
        let lines = transcript(8);
        let runs = topic_runs(&notes, 480_000);
        let written = vec![WrittenSection {
            start_ms: 0,
            title: "Pricing".into(),
            body: "- $40 per seat [00:00]".into(),
            questions: String::new(),
        }];
        let (sections, tail) = live_view(&runs, &written, &notes, &lines);
        let shape: Vec<(&str, &str, bool)> = sections
            .iter()
            .map(|s| (s.title.as_str(), s.body.as_str(), s.writing))
            .collect();
        assert_eq!(
            shape,
            [
                ("Pricing", "- $40 per seat [00:00]", false),
                ("Hiring", "", true),
                ("Roadmap", "", true),
            ]
        );
        assert_eq!(
            tail,
            "## Also discussed\n\n- Hello there everyone [03:00]\n\n## Decisions\n\n- Hire remote [07:00]"
        );
    }

    #[test]
    fn the_body_puts_the_users_notes_first_word_for_word() {
        let user = UserNote {
            ms: Some(0),
            text: "datadog?\n  - ask Sam".into(),
        };
        let reply = SectionReply {
            bullets: vec!["- Datadog not decided [01:00]".into()],
            questions: vec![],
        };
        assert_eq!(
            section_body(&[&user], &reply),
            "- datadog?\n  - ask Sam\n- Datadog not decided [01:00]"
        );
    }
}
