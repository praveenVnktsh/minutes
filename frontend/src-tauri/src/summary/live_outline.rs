//! Enhanced notes built from the live notes.
//!
//! When a meeting has live notes (`meeting_timeline_entries`, written by
//! `live_summary` while recording), they are the skeleton of the enhanced
//! notes:
//!
//! - every live-notes topic becomes a `## ` section, in order;
//! - each section is written by its own model call, from that topic's notes
//!   and only the transcript lines in the topic's time range (the transcript
//!   fills gaps and corrects the notes; it is not re-summarised from scratch);
//! - the user's typed notes are placed under the matching topic, by their
//!   timestamp when they have one, else by the words they share with it;
//! - `## Decisions`, `## Action items` and `## Open questions` are collected
//!   from the decision and action notes plus the follow-ups each section call
//!   reports;
//! - every bullet ends with a `[mm:ss]` link to the transcript.
//!
//! Everything here is pure except the [`SummaryPipeline`] methods at the end.
//!
//! **When the old path runs instead** ([`Fallback`]): no live notes, notes
//! without topics (made before topics existed), a transcript without
//! timestamps, or sparse notes: fewer than one per
//! [`MINUTES_PER_NOTE_LIMIT`] minutes of meeting. Sparse notes mean the live
//! summary was mostly off or failing, and a skeleton with holes would be
//! worse than the whole-transcript summary.
//!
//! **Blending** for local holes: a stretch longer than [`GAP_MS`] with no
//! note, before the first note, between two topics or after the last note,
//! becomes part of one `## Also discussed` section written from the
//! transcript alone, so nothing said there is lost or filed under the wrong
//! topic.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;

use once_cell::sync::Lazy;
use regex::Regex;
use tracing::{info, warn};

use crate::database::repositories::timeline::TimelineEntryRow;
use crate::live_summary::parse::is_repeat;
use crate::summary::budget::{chars_per_token, MIN_CHUNK_TOKENS};
use crate::summary::processor::{
    build_title_user_prompt, sanitize_generated_title, title_system_prompt, StageError,
    SummaryBackend, SummaryPipeline, ENGLISH_BASE_SUMMARY_INSTRUCTION, MAX_CHUNK_ATTEMPTS,
    SPEAKER_GUIDE,
};

/// Fewer live notes than one per this many minutes means the old path.
pub const MINUTES_PER_NOTE_LIMIT: i64 = 5;
/// A stretch this long without a note becomes "Also discussed".
pub const GAP_MS: i64 = 10 * 60_000;
/// How far a topic's range reaches into a gap next to it.
const GAP_PAD_MS: i64 = 60_000;
/// A gap with fewer transcript words than this is dropped (silence, small talk).
const MIN_GAP_WORDS: usize = 80;
/// The section holding transcript stretches without live notes.
pub const ALSO_DISCUSSED: &str = "Also discussed";
/// Output budget of one section call: a handful of bullets.
pub const SECTION_MAX_OUTPUT_TOKENS: usize = 1_536;
/// A link this far outside its section's time range is dropped.
const LINK_SLACK_MS: i64 = 60_000;

static CLOCK_PREFIX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\[(\d{1,3}:\d{2}(?::\d{2})?)\]").unwrap());
static NOTE_CLOCK_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\[(\d{1,3}:\d{2}(?::\d{2})?)\](?:\s+IMPORTANT)?(?:\s+(.*))?$").unwrap()
});
/// `[12:34]`, `[1:02:03]`, `(12:34)`, and ranges `[12:34-13:00]`, `[12:34–13:00]`.
static LINK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\[(](\d{1,3}:\d{2}(?::\d{2})?)(?:\s*[-–—]\s*\d{1,3}:\d{2}(?::\d{2})?)?[\])]")
        .unwrap()
});
static FOLLOW_UP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:[-*•]\s+)?\**(action(?:\s+item)?|decision|question|open question)s?\**\s*:\**\s*(.+)$")
        .unwrap()
});
static BULLET: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\s*)(?:[-*•+]|\d+[.)])\s+(.*)$").unwrap());

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

/// One live note, as the outline uses it.
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
        Self {
            start_ms,
            end_ms: end_ms.max(start_ms),
            kind: NoteKind::from_stored(kind),
            text: text.trim().to_string(),
            speaker: speaker
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            topic: topic
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string),
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

/// One of the user's typed notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserNote {
    /// When the user typed it, if the notes say (`[mm:ss]` live notes).
    pub ms: Option<i64>,
    /// The note as typed; sub-bullets follow on indented lines.
    pub text: String,
}

impl UserNote {
    fn first_line(&self) -> &str {
        self.text.lines().next().unwrap_or("").trim()
    }
}

/// One `## ` section of the enhanced notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    /// Transcript time ranges, `[start, end)`; a topic the meeting came back
    /// to has several.
    pub ranges: Vec<(i64, i64)>,
    pub notes: Vec<LiveNote>,
    pub user_notes: Vec<UserNote>,
    /// "Also discussed": transcript without live notes.
    pub is_gap: bool,
}

impl Section {
    pub fn contains(&self, ms: i64) -> bool {
        self.ranges
            .iter()
            .any(|(start, end)| ms >= *start && ms < *end)
    }

    /// First start and last end, the end capped at the meeting's length.
    pub fn span(&self, duration_ms: i64) -> (i64, i64) {
        let start = self.ranges.iter().map(|r| r.0).min().unwrap_or(0);
        let end = self.ranges.iter().map(|r| r.1).max().unwrap_or(0);
        (start, end.min(duration_ms.max(start)))
    }

    pub fn lines<'a>(&self, transcript: &'a [TranscriptLine]) -> Vec<&'a TranscriptLine> {
        transcript.iter().filter(|l| self.contains(l.ms)).collect()
    }

    fn distance(&self, ms: i64) -> i64 {
        self.ranges
            .iter()
            .map(|(start, end)| {
                if ms < *start {
                    start - ms
                } else if ms >= *end {
                    ms - end + 1
                } else {
                    0
                }
            })
            .min()
            .unwrap_or(i64::MAX)
    }
}

/// The skeleton of a meeting's enhanced notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    pub sections: Vec<Section>,
    /// Every live note, by time, with its topic filled in.
    pub notes: Vec<LiveNote>,
    pub transcript: Vec<TranscriptLine>,
    pub duration_ms: i64,
}

impl Outline {
    /// What the outline is built from, for the summary cache key.
    pub fn fingerprint_text(&self) -> String {
        let mut out = String::new();
        for section in &self.sections {
            let _ = writeln!(out, "## {} {:?}", section.title, section.ranges);
            for note in &section.notes {
                let _ = writeln!(
                    out,
                    "{} {:?} {:?} {}",
                    note.start_ms, note.kind, note.speaker, note.text
                );
            }
            for note in &section.user_notes {
                let _ = writeln!(out, "user {:?} {}", note.ms, note.text);
            }
        }
        out
    }
}

/// Why a meeting's notes are written the old way.
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

fn strip_markers(line: &str) -> &str {
    let line = line.trim().trim_start_matches('#').trim();
    BULLET
        .captures(line)
        .and_then(|c| c.get(2))
        .map_or(line, |m| m.as_str())
        .trim()
}

/// The user's notes, one per item: `[mm:ss]` blocks from the live notes
/// editor, or the lines (bullets, headings, paragraphs) of free-form notes.
/// Indented sub-bullets stay with the note above them.
pub fn parse_user_notes(notes: &str) -> Vec<UserNote> {
    let mut out: Vec<UserNote> = Vec::new();
    let mut in_timed_block = false;
    for raw in notes.lines() {
        if raw.trim().is_empty() {
            in_timed_block = false;
            continue;
        }
        let trimmed = raw.trim();
        if trimmed.chars().all(|c| matches!(c, '-' | '*' | '_' | '=')) {
            continue;
        }
        if let Some(captures) = NOTE_CLOCK_LINE.captures(trimmed) {
            let ms = parse_transcript_clock(&captures[1]);
            let text = captures.get(2).map_or("", |m| m.as_str()).trim();
            out.push(UserNote {
                ms,
                text: text.to_string(),
            });
            in_timed_block = true;
            continue;
        }
        let indented = raw.starts_with("  ") || raw.starts_with('\t');
        let text = strip_markers(raw);
        if text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if in_timed_block && last.text.is_empty() => last.text = text.to_string(),
            Some(last) if in_timed_block => {
                let _ = write!(last.text, " {text}");
            }
            Some(last) if indented => {
                let _ = write!(last.text, "\n  - {text}");
            }
            _ => out.push(UserNote {
                ms: None,
                text: text.to_string(),
            }),
        }
    }
    out.retain(|n| !n.text.trim().is_empty());
    out
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

/// Puts each user note under a section: a timed note under the section
/// whose range holds its time (else the nearest); an untimed note under the
/// section it shares the most words with (title ×3, live notes ×2,
/// transcript ×1), or with the note before it when it shares none.
pub fn place_user_notes(
    sections: &mut [Section],
    transcript: &[TranscriptLine],
    user_notes: Vec<UserNote>,
) {
    if sections.is_empty() {
        return;
    }
    let profiles: Vec<(HashSet<String>, HashSet<String>, HashSet<String>)> = sections
        .iter()
        .map(|section| {
            let notes = section
                .notes
                .iter()
                .map(|n| n.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let lines = section
                .lines(transcript)
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let title = if section.is_gap {
                HashSet::new()
            } else {
                content_words(&section.title)
            };
            (title, content_words(&notes), content_words(&lines))
        })
        .collect();
    let mut previous = 0;
    for note in user_notes {
        let index = match note.ms {
            Some(ms) => sections
                .iter()
                .enumerate()
                .min_by_key(|(i, s)| (s.distance(ms), *i))
                .map_or(0, |(i, _)| i),
            None => {
                let words = content_words(&note.text);
                let (best, score) = profiles
                    .iter()
                    .enumerate()
                    .map(|(i, (title, notes, lines))| {
                        let score = 3 * words.intersection(title).count()
                            + 2 * words.intersection(notes).count()
                            + words.intersection(lines).count();
                        (i, score)
                    })
                    // Highest score; the earliest section on a tie.
                    .fold((0, 0), |best, cur| if cur.1 > best.1 { cur } else { best });
                if score == 0 {
                    previous
                } else {
                    best
                }
            }
        };
        previous = index;
        sections[index].user_notes.push(note);
    }
}

fn same_topic(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Builds the outline, or says why the old path should run.
pub fn build_outline(
    entries: &[LiveNote],
    transcript_text: &str,
    user_notes: &str,
) -> Result<Outline, Fallback> {
    let mut notes: Vec<LiveNote> = entries
        .iter()
        .filter(|n| !n.text.is_empty())
        .cloned()
        .collect();
    if notes.is_empty() {
        return Err(Fallback::NoNotes);
    }
    notes.sort_by_key(|n| (n.start_ms, n.end_ms));
    let Some(first_topic) = notes.iter().find_map(|n| n.topic.clone()) else {
        return Err(Fallback::NoTopics);
    };
    let (transcript, timed) = parse_transcript(transcript_text);
    if timed == 0 {
        return Err(Fallback::NoTimestamps);
    }
    let last_line = transcript.iter().map(|l| l.ms).max().unwrap_or(0);
    let duration_ms = last_line.max(notes.iter().map(|n| n.end_ms).max().unwrap_or(0));
    let minutes = (duration_ms + 59_999) / 60_000;
    if (notes.len() as i64) * MINUTES_PER_NOTE_LIMIT < minutes {
        return Err(Fallback::Sparse {
            notes: notes.len(),
            minutes,
        });
    }

    // A note without a topic sits under the one before it.
    let mut current = first_topic;
    for note in &mut notes {
        match &note.topic {
            Some(topic) => current = topic.clone(),
            None => note.topic = Some(current.clone()),
        }
    }

    // Runs of consecutive notes under one topic.
    let mut runs: Vec<(String, Vec<LiveNote>)> = Vec::new();
    for note in &notes {
        let topic = note.topic.clone().unwrap_or_default();
        match runs.last_mut() {
            Some((title, run)) if same_topic(title, &topic) => run.push(note.clone()),
            _ => runs.push((topic, vec![note.clone()])),
        }
    }

    // Each run covers the transcript up to the next run's first note; a
    // stretch over GAP_MS without notes is cut out for "Also discussed".
    let end_of_meeting = duration_ms + 1;
    let mut gaps: Vec<(i64, i64)> = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut begin = 0;
    let first_start = runs[0].1[0].start_ms;
    if first_start > GAP_MS {
        gaps.push((0, first_start - GAP_PAD_MS));
        begin = first_start - GAP_PAD_MS;
    }
    for (index, (title, run)) in runs.iter().enumerate() {
        let last_note_end = run.iter().map(|n| n.end_ms).max().unwrap_or(begin);
        let next_start = runs
            .get(index + 1)
            .map_or(end_of_meeting, |(_, next)| next[0].start_ms);
        let (end, next_begin) = if next_start - last_note_end > GAP_MS {
            let cut = last_note_end + GAP_PAD_MS;
            let resume = if index + 1 < runs.len() {
                next_start - GAP_PAD_MS
            } else {
                end_of_meeting
            };
            gaps.push((cut, resume));
            (cut, resume)
        } else {
            (next_start, next_start)
        };
        let range = (begin, end.max(begin));
        match sections
            .iter_mut()
            .find(|s| !s.is_gap && same_topic(&s.title, title))
        {
            Some(section) => {
                section.ranges.push(range);
                section.notes.extend(run.iter().cloned());
            }
            None => sections.push(Section {
                title: title.clone(),
                ranges: vec![range],
                notes: run.clone(),
                user_notes: Vec::new(),
                is_gap: false,
            }),
        }
        begin = next_begin;
    }
    let gap_section = Section {
        title: ALSO_DISCUSSED.to_string(),
        ranges: gaps,
        notes: Vec::new(),
        user_notes: Vec::new(),
        is_gap: true,
    };
    let gap_words: usize = gap_section
        .lines(&transcript)
        .iter()
        .map(|l| l.text.split_whitespace().count())
        .sum();
    if gap_words >= MIN_GAP_WORDS {
        sections.push(gap_section);
    }

    place_user_notes(&mut sections, &transcript, parse_user_notes(user_notes));
    Ok(Outline {
        sections,
        notes,
        transcript,
        duration_ms,
    })
}

/// System prompt of one section call.
pub fn section_system_prompt() -> String {
    format!(
        r#"You are Minutes, an expert meeting note-taker. You write ONE section of a meeting's notes: the part of the meeting named in the request.

{SPEAKER_GUIDE}

`<live_notes>` are short notes taken live during this part of the meeting. They are the skeleton of the section. `<transcript>` is what was said in this part.

**METHOD:**
1. Write 2-6 short bullets (`- `) for this part. Start from the live notes, in order: fix them or add specifics (names, numbers, dates) from the transcript, and add anything important they missed. Never repeat a point.
2. End every bullet with the time of the transcript line it comes from, as `[mm:ss]` copied from that line.
3. Each item in `<my_notes>` is the user's own note. Keep it as its own bullet, word for word, where it fits, and add up to 2 indented sub-bullets (`  - `) with specifics from the transcript. Never turn a question the user wrote into a statement.
4. After the bullets, add follow-ups from this part, one per line, only when the transcript states them clearly:
   `Action: Owner — task (due: date) [mm:ss]` for a task someone took on. Owner is You, Others or a name the transcript gives. Write the due date only if one was stated.
   `Decision: what was agreed [mm:ss]` for an explicit agreement.
   `Question: the question [mm:ss]` for a question left unanswered.
   Skip follow-ups already in the live notes as ACTION or DECISION.
5. Use only what the transcript says. Never invent names, numbers or dates.
6. No headings, no title, no introduction. {ENGLISH_BASE_SUMMARY_INSTRUCTION}
7. Output ONLY the bullets and follow-up lines — no reasoning or meta-commentary.

**EXAMPLE** (shows the format only; never copy its content):
- Movers booked for 3 June; budget capped at $4,000 [02:10]
- Window desks still contested between design and sales [03:05]
Action: You — send the floor plan to everyone (due: Friday) [03:40]
Question: Who gets the window desks? [03:05]"#
    )
}

fn note_line(note: &LiveNote) -> String {
    let time = format_transcript_clock(note.start_ms);
    match note.kind {
        NoteKind::Point => format!("[{time}] {}", note.text),
        NoteKind::Decision => format!("[{time}] DECISION: {}", note.text),
        NoteKind::Action => format!(
            "[{time}] ACTION ({}): {}",
            note.speaker.as_deref().unwrap_or("Unassigned"),
            note.text
        ),
    }
}

/// User prompt of one section call (or one piece of a long section).
pub fn section_user_prompt(
    section: &Section,
    span: (i64, i64),
    notes: &[&LiveNote],
    user_notes: &[&UserNote],
    lines: &[&TranscriptLine],
) -> String {
    let mut prompt = String::new();
    let _ = writeln!(
        prompt,
        "Part of the meeting: {} ({}–{})\n",
        section.title,
        format_transcript_clock(span.0),
        format_transcript_clock(span.1)
    );
    prompt.push_str("<live_notes>\n");
    if notes.is_empty() {
        prompt.push_str("(none: no live notes were taken here; write 1-4 bullets for the important points only)\n");
    }
    for note in notes {
        prompt.push_str(&note_line(note));
        prompt.push('\n');
    }
    prompt.push_str("</live_notes>\n\n");
    if !user_notes.is_empty() {
        prompt.push_str("<my_notes>\n");
        for note in user_notes {
            let _ = writeln!(prompt, "- {}", note.text);
        }
        prompt.push_str("</my_notes>\n\n");
    }
    prompt.push_str("<transcript>\n");
    for line in lines {
        prompt.push_str(&line.text);
        prompt.push('\n');
    }
    prompt.push_str("</transcript>\n");
    prompt
}

/// A decision, action or open question.
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
    pub bullets: Vec<String>,
    pub actions: Vec<FollowUp>,
    pub decisions: Vec<FollowUp>,
    pub questions: Vec<FollowUp>,
}

impl SectionReply {
    fn extend(&mut self, other: SectionReply) {
        for bullet in other.bullets {
            if !self.bullets.contains(&bullet) {
                self.bullets.push(bullet);
            }
        }
        self.actions.extend(other.actions);
        self.decisions.extend(other.decisions);
        self.questions.extend(other.questions);
    }
}

fn normalized(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
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
        .trim_end_matches([',', ';'])
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

fn with_link(text: &str, ms: Option<i64>) -> String {
    match ms {
        Some(ms) => format!("{text} [{}]", format_transcript_clock(ms)),
        None => text.to_string(),
    }
}

fn split_owner(text: &str) -> (Option<String>, String) {
    for separator in [" — ", " – ", " - ", ": "] {
        if let Some((owner, task)) = text.split_once(separator) {
            let owner = owner.trim().trim_matches('*').trim();
            if !owner.is_empty() && owner.split_whitespace().count() <= 4 && !task.trim().is_empty()
            {
                return (Some(owner.to_string()), task.trim().to_string());
            }
        }
    }
    (None, text.trim().to_string())
}

/// Reads a section call's reply: bullets (links checked, a missing one added
/// from the best-matching line) and `Action:` / `Decision:` / `Question:`
/// follow-ups. Headings and introductions are dropped.
pub fn parse_section_reply(raw: &str, span: (i64, i64), lines: &[&TranscriptLine]) -> SectionReply {
    let mut reply = SectionReply::default();
    for raw_line in raw.lines() {
        if raw_line.trim().is_empty() {
            continue;
        }
        let trimmed = raw_line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("```") {
            continue;
        }
        if let Some(captures) = FOLLOW_UP.captures(trimmed) {
            let label = captures[1].to_lowercase();
            let (text, ms) = take_links(captures[2].trim().trim_matches('*').trim(), span);
            if text.is_empty() {
                continue;
            }
            let ms = ms.or_else(|| best_line(&text, lines));
            if label.starts_with("action") {
                let (owner, task) = split_owner(&text);
                reply.actions.push(FollowUp {
                    text: task,
                    owner,
                    ms,
                });
            } else if label.starts_with("decision") {
                reply.decisions.push(FollowUp {
                    text,
                    owner: None,
                    ms,
                });
            } else {
                reply.questions.push(FollowUp {
                    text,
                    owner: None,
                    ms,
                });
            }
            continue;
        }
        let (indent, body) = match BULLET.captures(raw_line) {
            Some(c) => (c[1].len() >= 2, c[2].trim().to_string()),
            // Prose: a bullet, unless it introduces the list.
            None if trimmed.ends_with(':') => continue,
            None => (false, trimmed.to_string()),
        };
        let (text, ms) = take_links(&body, span);
        if text.is_empty() {
            continue;
        }
        let ms = ms.or_else(|| {
            if indent {
                None
            } else {
                best_line(&text, lines)
            }
        });
        let bullet = with_link(&text, ms);
        reply.bullets.push(if indent {
            format!("  - {bullet}")
        } else {
            format!("- {bullet}")
        });
    }
    reply
}

/// Puts back any user note the model left out, ahead of the bullets.
pub fn ensure_user_notes(bullets: &mut Vec<String>, user_notes: &[&UserNote]) {
    let body = normalized(&bullets.join(" "));
    let missing: Vec<String> = user_notes
        .iter()
        .filter(|note| {
            let first = normalized(note.first_line());
            !first.is_empty() && !body.contains(&first)
        })
        .flat_map(|note| {
            let mut lines = note.text.lines();
            let head = format!("- {}", lines.next().unwrap_or("").trim());
            std::iter::once(head).chain(lines.map(str::to_string))
        })
        .collect();
    if !missing.is_empty() {
        bullets.splice(0..0, missing);
    }
}

/// A section's bullets without a model: its live notes as written.
pub fn fallback_reply(section: &Section) -> SectionReply {
    SectionReply {
        bullets: section
            .notes
            .iter()
            .map(|n| format!("- {}", with_link(&n.text, Some(n.start_ms))))
            .collect(),
        ..Default::default()
    }
}

fn push_unique(list: &mut Vec<FollowUp>, item: FollowUp) {
    if item.text.trim().is_empty() || list.iter().any(|other| is_repeat(&item.text, &other.text)) {
        return;
    }
    list.push(item);
}

fn owner_label(owner: Option<&str>) -> String {
    owner
        .map(|o| o.trim().trim_matches('*').trim())
        .filter(|o| !o.is_empty())
        .unwrap_or("Unassigned")
        .to_string()
}

/// The finished notes: the title, one section per topic, then decisions,
/// action items and open questions. Live-note decisions and actions come
/// first; a follow-up a section call reports is added unless it repeats one.
pub fn assemble(title: &str, outline: &Outline, replies: &[SectionReply]) -> String {
    let mut decisions = Vec::new();
    let mut actions = Vec::new();
    let mut questions = Vec::new();
    for note in &outline.notes {
        let item = FollowUp {
            text: note.text.clone(),
            owner: note.speaker.clone(),
            ms: Some(note.start_ms),
        };
        match note.kind {
            NoteKind::Decision => push_unique(&mut decisions, item),
            NoteKind::Action => push_unique(&mut actions, item),
            NoteKind::Point => {}
        }
    }
    for reply in replies {
        for item in &reply.decisions {
            push_unique(&mut decisions, item.clone());
        }
        for item in &reply.actions {
            push_unique(&mut actions, item.clone());
        }
        for item in &reply.questions {
            push_unique(&mut questions, item.clone());
        }
    }

    let mut out = format!("# {}\n", title.trim());
    for (section, reply) in outline.sections.iter().zip(replies) {
        if reply.bullets.is_empty() {
            continue;
        }
        let _ = write!(out, "\n## {}\n\n", section.title);
        for bullet in &reply.bullets {
            out.push_str(bullet);
            out.push('\n');
        }
    }
    if !decisions.is_empty() {
        out.push_str("\n## Decisions\n\n");
        for item in &decisions {
            let _ = writeln!(out, "- {}", with_link(&item.text, item.ms));
        }
    }
    if !actions.is_empty() {
        out.push_str("\n## Action items\n\n");
        for item in &actions {
            let _ = writeln!(
                out,
                "- **{}** — {}",
                owner_label(item.owner.as_deref()),
                with_link(&item.text, item.ms)
            );
        }
    }
    if !questions.is_empty() {
        out.push_str("\n## Open questions\n\n");
        for item in &questions {
            let _ = writeln!(out, "- {}", with_link(&item.text, item.ms));
        }
    }
    out
}

/// Lines grouped into pieces of at most `max_chars` characters, in order.
pub fn group_lines<'a>(
    lines: &[&'a TranscriptLine],
    max_chars: usize,
) -> Vec<Vec<&'a TranscriptLine>> {
    let mut groups: Vec<Vec<&TranscriptLine>> = Vec::new();
    let mut size = 0;
    for line in lines {
        let len = line.text.chars().count() + 1;
        match groups.last_mut() {
            Some(group) if size + len <= max_chars => group.push(line),
            _ => {
                groups.push(vec![line]);
                size = 0;
            }
        }
        size += len;
    }
    groups
}

/// The notes of a section that belong to a piece starting at `from`
/// and ending before `until` (None: the section's first or last piece).
fn piece_notes<'a>(
    section: &'a Section,
    from: Option<i64>,
    until: Option<i64>,
) -> (Vec<&'a LiveNote>, Vec<&'a UserNote>) {
    let inside = |ms: i64| from.is_none_or(|f| ms >= f) && until.is_none_or(|u| ms < u);
    let notes = section
        .notes
        .iter()
        .filter(|n| inside(n.start_ms))
        .collect();
    let user_notes = section
        .user_notes
        .iter()
        .filter(|n| match n.ms {
            Some(ms) => inside(ms),
            // Untimed notes go with the first piece.
            None => from.is_none(),
        })
        .collect();
    (notes, user_notes)
}

impl<B: SummaryBackend> SummaryPipeline<'_, B> {
    /// Pass 1 from the live notes: one call per section (more for a section
    /// too long for the context), then a title call. Returns the notes and the
    /// number of model calls. A section whose call fails keeps its live notes.
    pub(crate) async fn outline_notes(
        &self,
        outline: &Outline,
        transcript_text: &str,
        user_notes: &str,
    ) -> Result<(String, i64), StageError> {
        let system = section_system_prompt();
        let mut replies = Vec::with_capacity(outline.sections.len());
        let mut calls = 0_i64;
        let mut failed = 0;
        info!(
            "Writing enhanced notes from {} live notes in {} sections",
            outline.notes.len(),
            outline.sections.len()
        );
        for (index, section) in outline.sections.iter().enumerate() {
            let reply = match self.write_section(&system, section, outline).await {
                Ok((reply, count)) => {
                    calls += count;
                    reply
                }
                Err(StageError::Cancelled) => return Err(StageError::Cancelled),
                Err(error) => {
                    warn!(
                        "Section {}/{} failed ({error}); keeping its live notes",
                        index + 1,
                        outline.sections.len()
                    );
                    failed += 1;
                    fallback_reply(section)
                }
            };
            let mut reply = reply;
            let user: Vec<&UserNote> = section.user_notes.iter().collect();
            ensure_user_notes(&mut reply.bullets, &user);
            replies.push(reply);
        }
        if failed == outline.sections.len() {
            return Err(StageError::Failed(
                "Summary generation failed: the model could not write any section of the notes. Please retry.".to_string(),
            ));
        }
        let title = self
            .outline_title(transcript_text, user_notes, outline)
            .await?;
        calls += 1;
        Ok((assemble(&title, outline, &replies), calls))
    }

    /// The separate title call, as for "Regenerate title". A failed or
    /// unusable title falls back to the first topic.
    async fn outline_title(
        &self,
        transcript_text: &str,
        user_notes: &str,
        outline: &Outline,
    ) -> Result<String, StageError> {
        let fallback = outline
            .sections
            .first()
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "Meeting notes".to_string());
        let prompt = build_title_user_prompt(transcript_text, Some(user_notes));
        match self
            .complete_attempt("Title", title_system_prompt(), &prompt, false, 1)
            .await
        {
            Ok(cleaned) => Ok(sanitize_generated_title(&cleaned.markdown).unwrap_or(fallback)),
            Err(StageError::Cancelled) => Err(StageError::Cancelled),
            Err(error) => {
                warn!("Title call failed ({error}); naming the notes after the first topic");
                Ok(fallback)
            }
        }
    }

    /// One section: a single call when its transcript fits the budget, else
    /// pieces of whole lines that do, each with the notes of its time range.
    async fn write_section(
        &self,
        system: &str,
        section: &Section,
        outline: &Outline,
    ) -> Result<(SectionReply, i64), StageError> {
        let span = section.span(outline.duration_ms);
        let lines = section.lines(&outline.transcript);
        let mut queue: VecDeque<Vec<&TranscriptLine>> = VecDeque::new();
        match self.budget {
            None => queue.push_back(lines.clone()),
            Some(budget) => {
                let (all_notes, all_user) = piece_notes(section, None, None);
                let overhead = self
                    .backend
                    .count_prompt_tokens(
                        system,
                        &section_user_prompt(section, span, &all_notes, &all_user, &[]),
                    )
                    .await?;
                let input = budget.input_budget(overhead);
                if input < MIN_CHUNK_TOKENS {
                    return Err(StageError::Failed(format!(
                        "The notes for '{}' leave no room for its transcript",
                        section.title
                    )));
                }
                let joined = lines
                    .iter()
                    .map(|l| l.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let tokens = self.backend.count_tokens(&joined).await?;
                if tokens <= input {
                    queue.push_back(lines.clone());
                } else {
                    let ratio = chars_per_token(&joined, tokens);
                    let max_chars = crate::summary::processor::chars_for_tokens(input, ratio);
                    let groups = group_lines(&lines, max_chars);
                    info!(
                        "Section '{}' has {} transcript tokens over a {} budget; writing it in {} pieces",
                        section.title,
                        tokens,
                        input,
                        groups.len()
                    );
                    queue.extend(groups);
                }
            }
        }

        let mut reply = SectionReply::default();
        let mut calls = 0;
        let mut first_piece = true;
        while let Some(piece) = queue.pop_front() {
            let from = (!first_piece).then(|| piece.first().map_or(span.0, |l| l.ms));
            let until = queue.front().and_then(|next| next.first()).map(|l| l.ms);
            let (notes, user) = piece_notes(section, from, until);
            let prompt = section_user_prompt(section, span, &notes, &user, &piece);
            let mut outcome = None;
            for attempt in 1..=MAX_CHUNK_ATTEMPTS {
                match self
                    .complete_attempt("Notes section", system, &prompt, true, attempt)
                    .await
                {
                    Ok(cleaned) => {
                        outcome = Some(Ok(cleaned.markdown));
                        break;
                    }
                    Err(_) if self.backend.is_cancelled() => return Err(StageError::Cancelled),
                    Err(StageError::Cancelled) => return Err(StageError::Cancelled),
                    Err(error @ StageError::Truncated(_)) => return Err(error),
                    Err(StageError::ContextOverflow(error)) => {
                        if piece.len() < 2 {
                            return Err(StageError::Failed(format!(
                                "A transcript line in '{}' does not fit the model's context: {error}",
                                section.title
                            )));
                        }
                        warn!(
                            "Section '{}' overflowed the context; splitting a piece of {} lines",
                            section.title,
                            piece.len()
                        );
                        outcome = Some(Err(()));
                        break;
                    }
                    Err(error) if attempt < MAX_CHUNK_ATTEMPTS => {
                        warn!("Section '{}' failed ({error}); retrying", section.title);
                    }
                    Err(error) => return Err(error),
                }
            }
            match outcome {
                Some(Ok(markdown)) => {
                    calls += 1;
                    first_piece = false;
                    reply.extend(parse_section_reply(&markdown, span, &piece));
                }
                Some(Err(())) => {
                    let half = piece.len() / 2;
                    queue.push_front(piece[half..].to_vec());
                    queue.push_front(piece[..half].to_vec());
                }
                None => {}
            }
        }
        Ok((reply, calls))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(min: f64, kind: &str, text: &str, topic: Option<&str>) -> LiveNote {
        let start = (min * 60_000.0) as i64;
        let speaker = (kind == "action").then_some("You");
        LiveNote::new(start, start + 20_000, kind, text, speaker, topic)
    }

    fn transcript(minutes: i64) -> String {
        (0..minutes * 2)
            .map(|half| {
                let ms = half * 30_000;
                format!(
                    "[{}] [Others] talking point number {half} about item{half}",
                    format_transcript_clock(ms)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
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
    }

    #[test]
    fn topics_become_sections_in_order_with_contiguous_ranges() {
        let notes = vec![
            note(0.5, "point", "Intro of the team", Some("Team Intro")),
            note(2.0, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(4.0, "decision", "Ship annual plan first", Some("Pricing")),
            note(6.0, "point", "Hiring two engineers", Some("Hiring")),
        ];
        let outline = build_outline(&notes, &transcript(8), "").unwrap();
        let titles: Vec<&str> = outline.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Team Intro", "Pricing", "Hiring"]);
        assert_eq!(outline.sections[0].ranges, vec![(0, 120_000)]);
        assert_eq!(outline.sections[1].ranges, vec![(120_000, 360_000)]);
        assert_eq!(outline.sections[2].ranges[0].0, 360_000);
        assert_eq!(outline.sections[1].notes.len(), 2);
        // Every transcript line lands in exactly one section.
        let covered: usize = outline
            .sections
            .iter()
            .map(|s| s.lines(&outline.transcript).len())
            .sum();
        assert_eq!(covered, outline.transcript.len());
    }

    #[test]
    fn a_topic_the_meeting_returns_to_is_one_section_with_two_ranges() {
        let notes = vec![
            note(0.5, "point", "Budget is tight", Some("Budget")),
            note(2.0, "point", "Launch date moved", Some("Launch")),
            note(4.0, "point", "Budget review in May", Some("budget")),
        ];
        let outline = build_outline(&notes, &transcript(6), "").unwrap();
        assert_eq!(outline.sections.len(), 2);
        assert_eq!(outline.sections[0].ranges.len(), 2);
        assert_eq!(outline.sections[0].notes.len(), 2);
    }

    #[test]
    fn notes_without_a_topic_sit_under_the_previous_one() {
        let notes = vec![
            note(0.5, "point", "First note", None),
            note(1.0, "point", "Second note", Some("Roadmap")),
            note(2.0, "point", "Third note", None),
        ];
        let outline = build_outline(&notes, &transcript(3), "").unwrap();
        assert_eq!(outline.sections.len(), 1);
        assert_eq!(outline.sections[0].notes.len(), 3);
    }

    #[test]
    fn falls_back_without_notes_topics_timestamps_or_density() {
        assert_eq!(
            build_outline(&[], &transcript(5), ""),
            Err(Fallback::NoNotes)
        );
        let untopiced = vec![note(1.0, "point", "Something", None)];
        assert_eq!(
            build_outline(&untopiced, &transcript(5), ""),
            Err(Fallback::NoTopics)
        );
        let one = vec![note(1.0, "point", "Something", Some("Topic"))];
        assert_eq!(
            build_outline(&one, "[You] no times\n[Others] at all", ""),
            Err(Fallback::NoTimestamps)
        );
        // 2 notes over 30 minutes: fewer than one per five minutes.
        let sparse = vec![
            note(1.0, "point", "A", Some("T")),
            note(20.0, "point", "B", Some("T")),
        ];
        assert_eq!(
            build_outline(&sparse, &transcript(30), ""),
            Err(Fallback::Sparse {
                notes: 2,
                minutes: 30
            })
        );
        // Six notes over 30 minutes is enough.
        let enough: Vec<LiveNote> = (0..6)
            .map(|i| note(i as f64 * 5.0 + 0.5, "point", &format!("N{i}"), Some("T")))
            .collect();
        assert!(build_outline(&enough, &transcript(30), "").is_ok());
    }

    #[test]
    fn a_long_stretch_without_notes_becomes_also_discussed() {
        let mut notes: Vec<LiveNote> = (0..4)
            .map(|i| {
                note(
                    i as f64 + 0.5,
                    "point",
                    &format!("Early {i}"),
                    Some("Early"),
                )
            })
            .collect();
        notes.extend(
            (0..4).map(|i| note(20.0 + i as f64, "point", &format!("Late {i}"), Some("Late"))),
        );
        let outline = build_outline(&notes, &transcript(24), "").unwrap();
        let titles: Vec<&str> = outline.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Early", "Late", ALSO_DISCUSSED]);
        let gap = &outline.sections[2];
        assert!(gap.is_gap && gap.contains(10 * 60_000));
        assert!(!outline.sections[0].contains(10 * 60_000));
        assert!(outline.sections[1].contains(19 * 60_000 + 30_000));
    }

    #[test]
    fn user_notes_parse_timed_blocks_and_free_form_lines() {
        let timed =
            parse_user_notes("[01:30] IMPORTANT\nfollow up on pricing\n\n[03:00]\ndatadog?");
        assert_eq!(
            timed,
            vec![
                UserNote {
                    ms: Some(90_000),
                    text: "follow up on pricing".into()
                },
                UserNote {
                    ms: Some(180_000),
                    text: "datadog?".into()
                },
            ]
        );
        let free = parse_user_notes("# Prep\n- pricing tiers?\n  - ask about seats\n* hiring plan");
        let texts: Vec<&str> = free.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Prep", "pricing tiers?\n  - ask about seats", "hiring plan"]
        );
    }

    #[test]
    fn user_notes_go_under_the_topic_by_time_then_by_content() {
        let notes = vec![
            note(0.5, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(
                3.0,
                "point",
                "Two engineers to hire this quarter",
                Some("Hiring"),
            ),
        ];
        let user = "[04:00]\nasked about onboarding\n\nhiring engineers budget?\nwhat about it\n";
        // The timed note falls in Hiring; the free-form one shares "hiring"
        // and "engineers" with it; the last shares nothing and follows it.
        let mut text = transcript(6);
        text.push_str("\n[05:59] [You] seat pricing details");
        let outline = build_outline(&notes, &text, user).unwrap();
        assert!(outline.sections[0].user_notes.is_empty());
        let placed: Vec<&str> = outline.sections[1]
            .user_notes
            .iter()
            .map(|n| n.text.as_str())
            .collect();
        assert_eq!(
            placed,
            [
                "asked about onboarding",
                "hiring engineers budget?",
                "what about it"
            ]
        );
        let free = parse_user_notes("seat pricing?");
        let mut sections = outline.sections.clone();
        for s in &mut sections {
            s.user_notes.clear();
        }
        place_user_notes(&mut sections, &outline.transcript, free);
        assert_eq!(sections[0].user_notes.len(), 1);
    }

    #[test]
    fn section_prompt_carries_notes_user_notes_and_only_its_transcript() {
        let notes = vec![
            note(0.5, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(1.0, "action", "send the price sheet", Some("Pricing")),
            note(3.0, "point", "Two engineers to hire", Some("Hiring")),
        ];
        let outline = build_outline(&notes, &transcript(4), "[00:50]\nannual discount?").unwrap();
        let section = &outline.sections[0];
        let lines = section.lines(&outline.transcript);
        let span = section.span(outline.duration_ms);
        let (n, u) = piece_notes(section, None, None);
        let prompt = section_user_prompt(section, span, &n, &u, &lines);
        assert!(prompt.starts_with("Part of the meeting: Pricing (00:00–03:00)"));
        assert!(prompt.contains("[00:30] Pricing at $40 per seat"));
        assert!(prompt.contains("[01:00] ACTION (You): send the price sheet"));
        assert!(prompt.contains("<my_notes>\n- annual discount?\n</my_notes>"));
        assert!(prompt.contains("[02:30] [Others]"));
        assert!(!prompt.contains("[03:00] [Others]"));
        assert!(!prompt.contains("Two engineers"));
        let system = section_system_prompt();
        assert!(system.contains("[You]") && system.contains("[mm:ss]"));
    }

    #[test]
    fn replies_keep_bullets_check_links_and_collect_follow_ups() {
        let (lines, _) = parse_transcript(
            "[01:00] [Others] the annual plan costs forty dollars per seat\n[02:00] [You] I will send the deck by Friday",
        );
        let lines: Vec<&TranscriptLine> = lines.iter().collect();
        let raw = "Here are the notes:\n## Pricing\n- Annual plan at $40 per seat [01:00]\n  - Discount for 3 years\n- Seat count unknown (45:00)\n* Annual plan costs forty dollars [01:00-01:30]\nAction: You — send the deck (due: Friday) [02:00]\n**Decision:** go annual\nQuestion: How many seats?";
        let reply = parse_section_reply(raw, (0, 180_000), &lines);
        assert_eq!(
            reply.bullets,
            [
                "- Annual plan at $40 per seat [01:00]",
                "  - Discount for 3 years",
                "- Seat count unknown",
                "- Annual plan costs forty dollars [01:00]",
            ]
        );
        assert_eq!(
            reply.actions,
            [FollowUp {
                text: "send the deck (due: Friday)".into(),
                owner: Some("You".into()),
                ms: Some(120_000)
            }]
        );
        assert_eq!(reply.decisions[0].text, "go annual");
        assert_eq!(reply.questions[0].text, "How many seats?");
    }

    #[test]
    fn a_bullet_without_a_link_gets_the_best_matching_line() {
        let (lines, _) = parse_transcript(
            "[01:00] [Others] the annual plan costs forty dollars\n[02:00] [You] hiring two engineers soon",
        );
        let lines: Vec<&TranscriptLine> = lines.iter().collect();
        let reply = parse_section_reply("- Hiring two engineers", (0, 180_000), &lines);
        assert_eq!(reply.bullets, ["- Hiring two engineers [02:00]"]);
    }

    #[test]
    fn missing_user_notes_are_put_back_word_for_word() {
        let mut bullets = vec!["- Datadog? — not decided [01:00]".to_string()];
        let notes = [
            UserNote {
                ms: None,
                text: "datadog?".into(),
            },
            UserNote {
                ms: None,
                text: "clippy mode?\n  - ask Sam".into(),
            },
        ];
        let refs: Vec<&UserNote> = notes.iter().collect();
        ensure_user_notes(&mut bullets, &refs);
        assert_eq!(
            bullets,
            [
                "- clippy mode?",
                "  - ask Sam",
                "- Datadog? — not decided [01:00]"
            ]
        );
    }

    #[test]
    fn assembled_notes_have_topics_then_decisions_actions_and_questions() {
        let notes = vec![
            note(0.5, "point", "Pricing at $40 per seat", Some("Pricing")),
            note(
                1.0,
                "decision",
                "Ship the annual plan first",
                Some("Pricing"),
            ),
            note(2.0, "action", "send the price sheet", Some("Pricing")),
        ];
        let outline = build_outline(&notes, &transcript(3), "").unwrap();
        let reply = SectionReply {
            bullets: vec!["- Pricing at $40 per seat [00:30]".into()],
            actions: vec![
                FollowUp {
                    text: "send the price sheet".into(),
                    owner: Some("You".into()),
                    ms: Some(120_000),
                },
                FollowUp {
                    text: "book the follow-up call".into(),
                    owner: None,
                    ms: Some(150_000),
                },
            ],
            decisions: vec![],
            questions: vec![FollowUp {
                text: "Monthly plan pricing?".into(),
                owner: None,
                ms: None,
            }],
        };
        let markdown = assemble("Pricing Review", &outline, &[reply]);
        assert_eq!(
            markdown,
            "# Pricing Review\n\n## Pricing\n\n- Pricing at $40 per seat [00:30]\n\n## Decisions\n\n- Ship the annual plan first [01:00]\n\n## Action items\n\n- **You** — send the price sheet [02:00]\n- **Unassigned** — book the follow-up call [02:30]\n\n## Open questions\n\n- Monthly plan pricing?\n"
        );
    }

    #[test]
    fn long_sections_are_grouped_by_whole_lines() {
        let (lines, _) = parse_transcript(&transcript(3));
        let refs: Vec<&TranscriptLine> = lines.iter().collect();
        let groups = group_lines(&refs, 120);
        assert!(groups.len() > 1);
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), lines.len());
        assert!(groups
            .iter()
            .all(|g| g.iter().map(|l| l.text.len() + 1).sum::<usize>() <= 120 || g.len() == 1));
    }
}
