//! The notetaker rules applied to every parsed note. Pure.
//!
//! Small models drift back to narrating the call ("Others discuss…"), label
//! statements as actions, and rename the topic every minute. These rules hold
//! the line whatever the prompt achieved:
//!
//! - **Narration.** A note that opens with a speaker and a reporting verb
//!   ("Others discuss", "The speaker notes that", "You suggest") is rewritten
//!   to its content ("Uber's late entry into AI"); if too little is left it
//!   is dropped. A narrated note is never an action or a decision.
//! - **Actions** need an owner and a task. "You'll send the deck" becomes
//!   owner `You`, task "send the deck". Without an owner, or when the text is
//!   a statement rather than a task, the note becomes a point. A named owner
//!   missing from the text is put in front of it ("Priya — build the model").
//! - **Decisions** that are questions become points.
//! - **Speaker** is kept only as an action's owner.
//! - **Topics** are tidied (≤ 5 words, title-case-ish) and a near-duplicate
//!   of the current or an earlier topic reuses that topic's exact title.
//! - **Names** are spelled as the glossary's established spelling.

use super::glossary::Glossary;
use super::parse::{EntryKind, ParsedEntry};

/// Longest topic title kept, in words.
pub const MAX_TOPIC_WORDS: usize = 5;
/// A rewritten note needs at least this many words to stay.
const MIN_REWRITTEN_WORDS: usize = 3;

/// What the rules need besides the note itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoteContext<'a> {
    /// The latest note's topic.
    pub current_topic: Option<&'a str>,
    /// Earlier topics, most recent first.
    pub known_topics: &'a [String],
    /// Names seen so far.
    pub glossary: Option<&'a Glossary>,
    /// Earlier notes' texts, for the repeat check.
    pub previous_texts: &'a [String],
}

/// Applies the rules to one reply's notes in order, so a topic the first
/// note starts is the current topic for the next.
pub struct Refiner<'a> {
    glossary: Option<&'a Glossary>,
    current: Option<String>,
    /// Most recent first.
    topics: Vec<String>,
}

impl<'a> Refiner<'a> {
    pub fn new(context: &NoteContext<'a>) -> Self {
        Self {
            glossary: context.glossary,
            current: context.current_topic.map(str::to_string),
            topics: context.known_topics.to_vec(),
        }
    }

    /// The note under the rules, or `None` when nothing worth keeping is left.
    pub fn refine(&self, mut entry: ParsedEntry) -> Option<ParsedEntry> {
        let mut text = fix_acronyms(&trim_period(&entry.text));
        if let Some(glossary) = self.glossary {
            text = glossary.canonicalize(&text);
        }

        if entry.kind == EntryKind::Action {
            if let Some((owner, task)) = commitment(&text) {
                entry.speaker = Some(owner);
                text = task;
            }
        }

        match strip_narration(&text) {
            Narration::Clean => {}
            Narration::Rewritten(content) => {
                text = content;
                entry.kind = EntryKind::Point;
            }
            Narration::Drop => return None,
        }
        text = shorten(&text);

        entry.kind = match entry.kind {
            EntryKind::Action if !is_task(&text, entry.speaker.as_deref()) => EntryKind::Point,
            EntryKind::Decision if text.trim_end().ends_with('?') => EntryKind::Point,
            kind => kind,
        };
        if entry.kind == EntryKind::Action {
            let owner = entry.speaker.clone().unwrap_or_default();
            text = match owner.as_str() {
                "You" => lowercase_first_verb(&text),
                "Others" => text,
                name if !text.to_lowercase().contains(&name.to_lowercase()) => {
                    format!("{name} — {}", lowercase_first_verb(&text))
                }
                _ => text,
            };
        } else {
            entry.speaker = None;
        }

        entry.text = text;
        let proposed = entry
            .topic
            .as_deref()
            .and_then(normalize_topic)
            .map(|topic| fix_acronyms(&topic))
            .map(|topic| match self.glossary {
                Some(glossary) => glossary.canonicalize(&topic),
                None => topic,
            });
        entry.topic = resolve_topic(proposed, self.current.as_deref(), &self.topics);
        Some(entry)
    }

    /// A note was kept: its topic is now the current one.
    pub fn accept(&mut self, entry: &ParsedEntry) {
        if let Some(topic) = &entry.topic {
            self.topics.retain(|known| known != topic);
            self.topics.insert(0, topic.clone());
            self.current = Some(topic.clone());
        }
    }
}

/// Acronyms live transcription writes as words ("Mako Ai").
const ACRONYMS: &[(&str, &str)] = &[
    ("Ai", "AI"),
    ("Ml", "ML"),
    ("Av", "AV"),
    ("Gpu", "GPU"),
    ("Gpus", "GPUs"),
];

/// "Mako Ai" → "Mako AI": the acronyms above, as whole words.
fn fix_acronyms(text: &str) -> String {
    text.split(' ')
        .map(|piece| {
            let word = piece.trim_matches(|c: char| !c.is_alphanumeric());
            match ACRONYMS.iter().find(|(wrong, _)| *wrong == word) {
                Some((wrong, right)) => piece.replacen(wrong, right, 1),
                None => piece.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Notes over this many words are cut back to their first clause.
pub const LONG_NOTE_WORDS: usize = 16;
/// A first clause shorter than this is not a note on its own.
const MIN_CLAUSE_WORDS: usize = 5;

/// A run-on note ("A; B; C" past `LONG_NOTE_WORDS` words) keeps its leading
/// clauses up to the limit, as long as the first is a note on its own.
fn shorten(text: &str) -> String {
    if text.split_whitespace().count() <= LONG_NOTE_WORDS || !text.contains("; ") {
        return text.to_string();
    }
    let mut kept = String::new();
    for clause in text.split("; ") {
        let candidate = if kept.is_empty() {
            clause.to_string()
        } else {
            format!("{kept}; {clause}")
        };
        if !kept.is_empty() && candidate.split_whitespace().count() > LONG_NOTE_WORDS {
            break;
        }
        kept = candidate;
    }
    if kept.split_whitespace().count() < MIN_CLAUSE_WORDS {
        return text.to_string();
    }
    kept
}

fn trim_period(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.strip_suffix('.') {
        Some(rest) if !rest.ends_with('.') => rest.trim_end().to_string(),
        _ => trimmed.to_string(),
    }
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// "Send the deck" → "send the deck"; names and acronyms ("CMU", "Priya")
/// would not start a task, but are left alone just in case.
fn lowercase_first_verb(text: &str) -> String {
    let first = text.split_whitespace().next().unwrap_or("");
    let rest_lower = first.chars().skip(1).all(|c| !c.is_uppercase());
    let looks_like_verb = rest_lower && first.chars().count() > 1;
    if !looks_like_verb {
        return text.to_string();
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
                .replace('’', "'")
                .to_lowercase()
        })
        .collect()
}

/// Text after skipping `count` whitespace-separated words.
fn after_words(text: &str, count: usize) -> &str {
    let mut rest = text.trim_start();
    for _ in 0..count {
        rest = rest
            .find(char::is_whitespace)
            .map(|i| rest[i..].trim_start())
            .unwrap_or("");
    }
    rest.trim_start_matches([':', ',', '-', '—', '–', ';'])
        .trim_start()
}

/// Who speaks of themselves: "You", "I", "We".
const FIRST_PERSON: &[&str] = &["you", "i", "we"];

/// "You'll send the deck", "I will ask Priya", "You to follow up": the owner
/// and the task. Only first person; a named owner keeps the model's speaker.
fn commitment(text: &str) -> Option<(String, String)> {
    let words = words(text);
    let first = words.first()?.as_str();
    let (subject, contracted) = match first {
        "you'll" | "i'll" | "we'll" => (first.split('\'').next()?, true),
        _ if FIRST_PERSON.contains(&first) => (first, false),
        _ => return None,
    };
    let skip = if contracted {
        1
    } else {
        let after: Vec<&str> = words[1..].iter().map(String::as_str).collect();
        match after.as_slice() {
            ["will" | "to" | "should" | "must", ..] => 2,
            ["are" | "am" | "is", "going", "to", ..] => 4,
            ["plan" | "plans" | "offer" | "offers" | "offered" | "agree" | "agreed" | "agrees"
            | "promise" | "promised" | "need" | "needs" | "volunteer" | "volunteered", "to", ..] => {
                3
            }
            _ => return None,
        }
    };
    let task = after_words(text, skip).to_string();
    if words.len() <= skip || task.is_empty() {
        return None;
    }
    let owner = if subject == "we" { "Others" } else { "You" };
    // "We will" is the group: no single owner, so not "You".
    Some((owner.to_string(), task))
}

/// First words that start a statement, never a task.
const NOT_A_TASK: &[&str] = &[
    "the", "a", "an", "this", "that", "these", "those", "it", "its", "it's", "there", "goal",
    "plan", "team", "data", "why", "how", "what", "whether", "is", "are", "was", "were", "not",
    "no",
];

/// An action needs an owner and a task (not a statement).
fn is_task(text: &str, owner: Option<&str>) -> bool {
    if owner.is_none() {
        return false;
    }
    let first = words(text).into_iter().next().unwrap_or_default();
    !first.is_empty()
        && !NOT_A_TASK.contains(&first.as_str())
        && !first.chars().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Narration {
    /// The note states content.
    Clean,
    /// It narrated who spoke; this is the content alone.
    Rewritten(String),
    /// It narrated who spoke and nothing worth keeping is left.
    Drop,
}

/// Speakers a narrating note opens with, longest first.
const NARRATORS: &[&str] = &[
    "the other participants",
    "other participants",
    "the participants",
    "participants",
    "the others",
    "others",
    "the speakers",
    "the speaker",
    "speakers",
    "speaker",
    "someone",
    "everyone",
    "the group",
    "they",
    "he",
    "she",
    "you",
    "we",
    "i",
    "both",
];

/// Reporting verbs, as stems: "discuss" matches discuss, discusses, discussed.
const REPORTING: &[&str] = &[
    "discuss",
    "note",
    "explain",
    "mention",
    "describ",
    "say",
    "said",
    "share",
    "suggest",
    "clarif",
    "emphasi",
    "express",
    "prais",
    "cite",
    "confirm",
    "compar",
    "ask",
    "talk",
    "highlight",
    "point",
    "state",
    "outlin",
    "recall",
    "reflect",
    "introduc",
    "add",
    "argu",
    "observ",
    "recommend",
    "propos",
    "believ",
    "think",
    "thought",
    "feel",
    "felt",
    "rais",
    "question",
    "wonder",
    "acknowledg",
    "admit",
    "jok",
    "reiterat",
    "stress",
    "walk",
    "elaborat",
    "consider",
    "report",
    "indicat",
    "remark",
    "respond",
    "repl",
    "answer",
    "encourag",
    "advis",
    "warn",
    "recount",
    "detail",
    "tell",
    "told",
    "touch",
    "bring",
    "brought",
    "giv",
    "gave",
    "provid",
    "present",
    "explor",
    "review",
    "agree",
    "learn",
    "hear",
    "heard",
];

/// Words between a reporting verb and the content.
const PARTICLES: &[&str] = &[
    "that", "about", "out", "on", "upon", "through", "up", "over", "back", "in", "to", "also",
];

/// Narrators that are only ever a speaker label, never a note's subject.
const LABEL_NARRATORS: &[&str] = &[
    "the other participants",
    "other participants",
    "the participants",
    "participants",
    "the others",
    "others",
    "the speaker",
    "speaker",
];

/// Words after a speaker label that make it a noun phrase ("Others in the
/// group…"), not narration.
const FUNCTION_WORDS: &[&str] = &[
    "of", "for", "and", "or", "with", "in", "on", "at", "from", "to", "who", "that", "like",
    "than", "the", "a", "an",
];

/// Verbs after a speaker label that describe the person, not the content.
const BEING: &[&str] = &["is", "was", "are", "were", "has", "had", "seems", "seemed"];

/// How many words of `lower` the first matching narrator in `list` spans.
fn narrator_len(lower: &[String], list: &[&str]) -> Option<usize> {
    list.iter().find_map(|narrator| {
        let parts: Vec<&str> = narrator.split(' ').collect();
        (lower.len() > parts.len() && lower.iter().zip(&parts).all(|(w, p)| w == p))
            .then_some(parts.len())
    })
}

/// Whether a note opens with a speaker ("Others…", "The speaker…",
/// "They…"), however it goes on.
pub fn opens_with_speaker(text: &str) -> bool {
    narrator_len(&words(text), NARRATORS).is_some()
}

/// Whether `text` opens by narrating who spoke, and what is left without it.
///
/// - A speaker and a reporting verb ("Others discuss…", "They note that…")
///   become the content after them.
/// - A speaker label ("Others", "Speaker") and any other verb ending in -s or
///   -ed ("Speaker holds a robotics background") lose the label.
/// - A speaker label and a verb of being ("Others are excited…") describe
///   the person: dropped.
pub fn strip_narration(text: &str) -> Narration {
    let lower = words(text);
    let Some(subject_len) = narrator_len(&lower, NARRATORS) else {
        return Narration::Clean;
    };
    let label = narrator_len(&lower, LABEL_NARRATORS).is_some();
    let mut index = subject_len;
    // "Speaker 2", "Speaker B".
    if lower[index - 1].starts_with("speaker")
        && lower
            .get(index)
            .is_some_and(|w| w.len() == 1 || w.chars().all(|c| c.is_ascii_digit()))
    {
        index += 1;
    }
    // "Others also note", "The speaker briefly explains".
    while lower
        .get(index)
        .is_some_and(|w| w == "also" || w == "then" || (w.ends_with("ly") && w.len() > 3))
    {
        index += 1;
    }
    let Some(verb) = lower.get(index) else {
        return Narration::Clean;
    };
    let reporting = REPORTING
        .iter()
        .any(|stem| verb.starts_with(stem) && verb.len() <= stem.len() + 3);
    if !reporting {
        if !label {
            return Narration::Clean;
        }
        if BEING.contains(&verb.as_str()) {
            return Narration::Drop;
        }
        // "Others seek…" (plural) or "Speaker holds…": any verb after a label.
        let plural_label = lower[subject_len - 1] != "speaker";
        let verb_like = verb.chars().all(char::is_alphabetic)
            && !FUNCTION_WORDS.contains(&verb.as_str())
            && (plural_label || (verb.len() > 3 && (verb.ends_with('s') || verb.ends_with("ed"))));
        if verb_like {
            let content = after_words(text, index);
            return if content.split_whitespace().count() <= MIN_REWRITTEN_WORDS {
                Narration::Drop
            } else {
                Narration::Rewritten(capitalize_first(content))
            };
        }
        return Narration::Clean;
    }
    index += 1;
    while lower
        .get(index)
        .is_some_and(|w| PARTICLES.contains(&w.as_str()))
    {
        index += 1;
    }
    let content = after_words(text, index);
    if content.split_whitespace().count() < MIN_REWRITTEN_WORDS {
        return Narration::Drop;
    }
    Narration::Rewritten(capitalize_first(content))
}

/// Words a title keeps lowercase after its first word.
const SMALL_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "of", "to", "in", "on", "for", "with", "at", "by", "vs",
    "via", "from", "as",
];

/// A tidy topic title: no quotes, labels or end punctuation, at most
/// `MAX_TOPIC_WORDS` words, first letters capitalized except small words.
/// Words with capitals of their own ("AI", "iOS") are kept as written.
pub fn normalize_topic(raw: &str) -> Option<String> {
    // "Old topic | New topic": the model echoing a path; the last part is meant.
    let raw = raw
        .rsplit(['|', '>'])
        .find(|part| !part.trim().is_empty())
        .unwrap_or(raw);
    let mut text = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '*' || c == '#' || c == '`')
        .trim()
        .to_string();
    if let Some(rest) = text
        .strip_prefix("Topic:")
        .or_else(|| text.strip_prefix("topic:"))
    {
        text = rest.trim().to_string();
    }
    let text = text.trim_end_matches(['.', ':', ';', ',', '!', '?', '-', '—']);
    if matches!(
        text.to_lowercase().as_str(),
        "" | "null" | "none" | "n/a" | "general" | "misc" | "other" | "topic" | "..."
    ) {
        return None;
    }
    let mut parts: Vec<&str> = text.split(' ').take(MAX_TOPIC_WORDS).collect();
    while parts.len() > 1
        && parts
            .last()
            .is_some_and(|w| SMALL_WORDS.contains(&w.to_lowercase().as_str()))
    {
        parts.pop();
    }
    let titled: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(index, word)| {
            let lower = word.to_lowercase();
            let has_own_capitals = word.chars().skip(1).any(char::is_uppercase);
            if has_own_capitals {
                word.to_string()
            } else if index > 0 && SMALL_WORDS.contains(&lower.as_str()) {
                lower
            } else {
                capitalize_first(word)
            }
        })
        .collect();
    Some(titled.join(" ")).filter(|t| !t.is_empty())
}

fn topic_words(topic: &str) -> Vec<String> {
    words(topic)
        .into_iter()
        .filter(|w| !w.is_empty() && !SMALL_WORDS.contains(&w.as_str()))
        .map(|w| w.trim_end_matches("'s").trim_end_matches('s').to_string())
        .collect()
}

/// Whether two titles name the same subject: same content words, one's
/// words within the other's, or at least half of them shared.
pub fn same_topic(a: &str, b: &str) -> bool {
    let (wa, wb) = (topic_words(a), topic_words(b));
    if wa.is_empty() || wb.is_empty() {
        return a.eq_ignore_ascii_case(b);
    }
    let shared = wa.iter().filter(|w| wb.contains(w)).count();
    let union = wa.len() + wb.len() - shared;
    shared == wa.len() || shared == wb.len() || shared * 2 >= union
}

/// Title words too general to tie two topics together.
const GENERIC_TOPIC_WORDS: &[&str] = &[
    "team",
    "plan",
    "strategy",
    "discussion",
    "overview",
    "update",
    "data",
    "size",
    "scope",
    "comparison",
    "approach",
    "review",
    "detail",
    "background",
    "proces",
    "work",
    "project",
    "company",
    "goal",
    "next",
    "step",
    "issue",
    "question",
    "idea",
    "general",
    "new",
    "current",
    "role",
    "experience",
    "growth",
    "focu",
    "priorite",
    "prioritie",
    "priority",
    "timeline",
];

/// Whether a proposed title continues the current topic: a near-duplicate,
/// or sharing a specific word with it ("Acme Pilot Scope" continues "Acme
/// Pilot"; "Acme Hiring" continues "Acme Partnership"). Small models coin a
/// new title for every detail; a section should last for a subject.
pub fn continues_topic(proposed: &str, current: &str) -> bool {
    if same_topic(proposed, current) {
        return true;
    }
    let current_words = topic_words(current);
    topic_words(proposed).iter().any(|word| {
        word.len() > 1
            && !GENERIC_TOPIC_WORDS.contains(&word.as_str())
            && current_words.contains(word)
    })
}

/// The topic a note goes under: the current topic when none is proposed or
/// the proposal continues it, an earlier topic's exact title when the
/// proposal is a near-duplicate of one, else the proposal (a new topic).
pub fn resolve_topic(
    proposed: Option<String>,
    current: Option<&str>,
    known: &[String],
) -> Option<String> {
    let Some(proposed) = proposed else {
        return current.map(str::to_string);
    };
    if let Some(current) = current.filter(|current| continues_topic(&proposed, current)) {
        return Some(current.to_string());
    }
    if let Some(earlier) = known.iter().find(|known| same_topic(&proposed, known)) {
        return Some(earlier.clone());
    }
    Some(proposed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(
        kind: EntryKind,
        text: &str,
        speaker: Option<&str>,
        topic: Option<&str>,
    ) -> ParsedEntry {
        ParsedEntry {
            start_ms: 0,
            end_ms: 1_000,
            kind,
            text: text.into(),
            speaker: speaker.map(str::to_string),
            topic: topic.map(str::to_string),
        }
    }

    fn refine(entry: ParsedEntry) -> Option<ParsedEntry> {
        Refiner::new(&NoteContext::default()).refine(entry)
    }

    #[test]
    fn narration_is_rewritten_to_its_content() {
        let cases = [
            (
                "Others discuss the late entry into the AI market.",
                "The late entry into the AI market.",
            ),
            (
                "Speaker notes current team is effectively a startup",
                "Current team is effectively a startup",
            ),
            (
                "The speaker explains that prioritization differs by scale",
                "Prioritization differs by scale",
            ),
            (
                "Others describe Acme, a seed-stage firm with 70 people",
                "Acme, a seed-stage firm with 70 people",
            ),
            (
                "They also point out that budgets are frozen until May",
                "Budgets are frozen until May",
            ),
            (
                "Speaker 2 briefly mentions the hiring freeze in Berlin",
                "The hiring freeze in Berlin",
            ),
            (
                "You suggest connecting with founders in the network",
                "Connecting with founders in the network",
            ),
            (
                "Others discuss why the team chose a startup",
                "Why the team chose a startup",
            ),
            (
                "Speaker holds a robotics background from a big lab",
                "Holds a robotics background from a big lab",
            ),
            (
                "Speaker 2 joined the company in 2019 as an intern",
                "Joined the company in 2019 as an intern",
            ),
            (
                "Others seek startup roles after graduating next spring",
                "Seek startup roles after graduating next spring",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                strip_narration(input),
                Narration::Rewritten(expected.into()),
                "{input}"
            );
        }
    }

    #[test]
    fn narration_with_nothing_left_is_dropped_and_content_is_untouched() {
        assert_eq!(strip_narration("Others discuss this"), Narration::Drop);
        assert_eq!(strip_narration("Speaker agrees."), Narration::Drop);
        // A label and a verb of being describes the person, not the content.
        assert_eq!(
            strip_narration("Others are excited about the new office"),
            Narration::Drop
        );
        assert!(opens_with_speaker(
            "Others are excited about the new office"
        ));
        assert!(!opens_with_speaker("Team grew to 100 people"));
        for clean in [
            "They hold weekly syncs with sales",
            "Others in the group want a Friday launch",
            "Team grew from ~30 to nearly 100 people",
            "Others' roadmap slips to Q3",
            "They plan to open an office in Austin",
            "Speakers for the summit are confirmed",
            "Budget review moves to Friday",
        ] {
            assert_eq!(strip_narration(clean), Narration::Clean, "{clean}");
        }
    }

    #[test]
    fn transcribed_acronyms_are_capitalized() {
        assert_eq!(
            fix_acronyms("Mako Ai raised a Series B (Ai, Ml)"),
            "Mako AI raised a Series B (AI, ML)"
        );
        assert_eq!(
            fix_acronyms("Aim for 4 Gpus; said Ainsley"),
            "Aim for 4 GPUs; said Ainsley"
        );
        let refined = refine(note(
            EntryKind::Point,
            "Mako Ai has 70 people",
            None,
            Some("mako ai"),
        ))
        .unwrap();
        assert_eq!(refined.text, "Mako AI has 70 people");
        assert_eq!(refined.topic.as_deref(), Some("Mako AI"));
    }

    #[test]
    fn run_on_notes_keep_their_leading_clauses() {
        let long = "Team grew from ~40 to nearly 100 people; works across the full stack from hardware to data; hiring more";
        assert_eq!(shorten(long), "Team grew from ~40 to nearly 100 people");
        let two = "Ten people run ~10 PB a day; own hardware stack; customers overseas and an office opening soon";
        assert_eq!(
            shorten(two),
            "Ten people run ~10 PB a day; own hardware stack"
        );
        // Short notes, notes without clauses and too-short first clauses stay.
        assert_eq!(
            shorten("Budget fine; hiring paused"),
            "Budget fine; hiring paused"
        );
        let no_clause = "word ".repeat(20);
        assert_eq!(shorten(&no_clause), no_clause);
        let tiny = "Yes; the budget for the new office in Austin was approved by finance after a long review";
        assert_eq!(shorten(tiny), tiny);
    }

    #[test]
    fn a_narrated_note_is_never_an_action_or_decision() {
        let refined = refine(note(
            EntryKind::Action,
            "Others clarify this is a strategic bet, not a moonshot",
            Some("Others"),
            None,
        ))
        .unwrap();
        assert_eq!(refined.kind, EntryKind::Point);
        assert_eq!(refined.text, "This is a strategic bet, not a moonshot");
        assert_eq!(refined.speaker, None);
        assert!(refine(note(EntryKind::Point, "Others discuss it", None, None)).is_none());
    }

    #[test]
    fn first_person_commitments_become_owned_tasks() {
        let cases = [
            (
                "You'll send the deck to support today.",
                "send the deck to support today",
            ),
            (
                "I will ask Priya for the pricing model",
                "ask Priya for the pricing model",
            ),
            (
                "You plan to Connect with founders next week",
                "connect with founders next week",
            ),
            ("You are going to draft the memo", "draft the memo"),
        ];
        for (input, task) in cases {
            let refined = refine(note(EntryKind::Action, input, None, None)).unwrap();
            assert_eq!(refined.kind, EntryKind::Action, "{input}");
            assert_eq!(refined.speaker.as_deref(), Some("You"), "{input}");
            assert_eq!(refined.text, task, "{input}");
        }
        let group = refine(note(
            EntryKind::Action,
            "We will revisit pricing in May",
            None,
            None,
        ))
        .unwrap();
        assert_eq!(
            (group.kind, group.speaker.as_deref()),
            (EntryKind::Action, Some("Others"))
        );
    }

    #[test]
    fn actions_need_an_owner_and_a_task() {
        // No owner.
        let unowned = refine(note(
            EntryKind::Action,
            "Ship the beta on Friday",
            None,
            None,
        ))
        .unwrap();
        assert_eq!(unowned.kind, EntryKind::Point);
        // A statement, not a task.
        for statement in [
            "Goal: put kits on drivers' cars next year",
            "The team is a startup",
        ] {
            let refined = refine(note(EntryKind::Action, statement, Some("Others"), None)).unwrap();
            assert_eq!(refined.kind, EntryKind::Point, "{statement}");
            assert_eq!(refined.speaker, None);
        }
        // An owned task stays, with a You task starting lowercase.
        let owned = refine(note(
            EntryKind::Action,
            "Connect with founders",
            Some("You"),
            None,
        ))
        .unwrap();
        assert_eq!(
            (owned.kind, owned.text.as_str()),
            (EntryKind::Action, "connect with founders")
        );
        // A named owner missing from the text is put in front.
        let named = refine(note(
            EntryKind::Action,
            "Build the pricing model",
            Some("Priya"),
            None,
        ))
        .unwrap();
        assert_eq!(named.text, "Priya — build the pricing model");
        let present = refine(note(
            EntryKind::Action,
            "Priya builds the pricing model",
            Some("Priya"),
            None,
        ))
        .unwrap();
        assert_eq!(present.text, "Priya builds the pricing model");
    }

    #[test]
    fn questions_are_not_decisions_and_points_drop_their_speaker() {
        let question = refine(note(
            EntryKind::Decision,
            "Launch on the tenth?",
            Some("Others"),
            None,
        ))
        .unwrap();
        assert_eq!(question.kind, EntryKind::Point);
        let decision = refine(note(
            EntryKind::Decision,
            "Launch moves to the 10th.",
            Some("Others"),
            None,
        ))
        .unwrap();
        assert_eq!(decision.kind, EntryKind::Decision);
        assert_eq!(decision.text, "Launch moves to the 10th");
        assert_eq!(decision.speaker, None);
    }

    #[test]
    fn topics_are_tidied() {
        assert_eq!(
            normalize_topic("  \"launch   plan.\" ").as_deref(),
            Some("Launch Plan")
        );
        assert_eq!(
            normalize_topic("Topic: pricing for the enterprise tier and more").as_deref(),
            Some("Pricing for the Enterprise Tier")
        );
        assert_eq!(
            normalize_topic("AI data at scale").as_deref(),
            Some("AI Data at Scale")
        );
        assert_eq!(normalize_topic("state of the").as_deref(), Some("State"));
        assert_eq!(
            normalize_topic("iOS release").as_deref(),
            Some("iOS Release")
        );
        assert_eq!(
            normalize_topic("Launch plan | Pricing tiers").as_deref(),
            Some("Pricing Tiers")
        );
        assert_eq!(normalize_topic("null"), None);
        assert_eq!(normalize_topic("  "), None);
    }

    #[test]
    fn near_duplicate_topics_reuse_the_existing_title() {
        let known = vec!["Launch Plan".to_string(), "Beta Feedback".to_string()];
        let resolve = |proposed: &str, current: Option<&str>| {
            resolve_topic(normalize_topic(proposed), current, &known)
        };
        assert_eq!(
            resolve("launch plans", Some("Launch Plan")).as_deref(),
            Some("Launch Plan")
        );
        assert_eq!(
            resolve("Launch plan timing", Some("Launch Plan")).as_deref(),
            Some("Launch Plan")
        );
        assert_eq!(
            resolve("The beta feedback", Some("Launch Plan")).as_deref(),
            Some("Beta Feedback")
        );
        assert_eq!(
            resolve("Pricing", Some("Launch Plan")).as_deref(),
            Some("Pricing")
        );
        // A specific shared word continues the current topic...
        assert_eq!(
            resolve("Acme pilot scope", Some("Acme Partnership")).as_deref(),
            Some("Acme Partnership")
        );
        // ...a generic one does not.
        assert_eq!(
            resolve("Hiring plan", Some("Launch Plan")).as_deref(),
            Some("Hiring Plan")
        );
        // Earlier topics need a near-duplicate, not one shared word.
        assert_eq!(
            resolve("Beta rollout", Some("Pricing")).as_deref(),
            Some("Beta Rollout")
        );
        // No topic proposed: the note stays under the current one.
        assert_eq!(
            resolve_topic(None, Some("Launch Plan"), &known).as_deref(),
            Some("Launch Plan")
        );
        assert_eq!(resolve_topic(None, None, &[]), None);
    }

    #[test]
    fn a_reply_that_starts_a_topic_keeps_its_later_notes_under_it() {
        let context = NoteContext {
            current_topic: Some("Launch Plan"),
            ..Default::default()
        };
        let mut refiner = Refiner::new(&context);
        let first = refiner
            .refine(note(
                EntryKind::Point,
                "Freemium tier held back a month",
                None,
                Some("pricing tiers"),
            ))
            .unwrap();
        assert_eq!(first.topic.as_deref(), Some("Pricing Tiers"));
        refiner.accept(&first);
        let second = refiner
            .refine(note(
                EntryKind::Point,
                "Metering is two sprints out",
                None,
                Some("Pricing tier"),
            ))
            .unwrap();
        assert_eq!(second.topic.as_deref(), Some("Pricing Tiers"));
        let third = refiner
            .refine(note(EntryKind::Point, "Dates confirmed", None, None))
            .unwrap();
        assert_eq!(third.topic.as_deref(), Some("Pricing Tiers"));
    }

    #[test]
    fn names_follow_the_glossary() {
        let glossary =
            Glossary::from_texts(["Pilot with Mako signed", "Office for Mako in Austin"]);
        let context = NoteContext {
            glossary: Some(&glossary),
            ..Default::default()
        };
        let refined = Refiner::new(&context)
            .refine(note(
                EntryKind::Point,
                "MACO processes ~10 TB a day",
                None,
                Some("MACO growth"),
            ))
            .unwrap();
        assert_eq!(refined.text, "Mako processes ~10 TB a day");
        assert_eq!(refined.topic.as_deref(), Some("Mako Growth"));
    }
}
