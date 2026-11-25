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
//! - **Length.** A note is at most `MAX_NOTE_WORDS` words: cut back to its
//!   leading clauses, or at a clause word, never mid-phrase.
//! - **Numbers** must appear in the transcript the note was written from
//!   (`facts.rs`). A figure from elsewhere is taken out of the note (its
//!   clause, or the figure and the words that only qualify it); a note left
//!   with too little is dropped.

use super::facts::Figures;
use super::glossary::Glossary;
use super::parse::{EntryKind, ParsedEntry, TopicDecision};

/// Longest topic title kept, in words.
pub const MAX_TOPIC_WORDS: usize = 5;
/// A rewritten note needs at least this many words to stay.
const MIN_REWRITTEN_WORDS: usize = 3;

/// With [`NoteContext::drop_fragments`], points shorter than this say
/// nothing on their own and are dropped.
pub const MIN_NOTE_WORDS: usize = 3;
/// Actions and decisions are short by nature ("Email Sarah", "Budget
/// approved"): two words are enough.
pub const MIN_ACTION_WORDS: usize = 2;

/// Last words that leave a two-word note pointing at nothing ("do that").
const VAGUE_OBJECTS: &[&str] = &["that", "this", "it", "them", "so", "one", "those", "these"];

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
    /// The figures of the transcript the notes were written from. `None`
    /// skips the number check.
    pub figures: Option<&'a Figures>,
    /// The window's topic from the topic step (`decide_topic`): every note
    /// of the window takes it. `None` resolves the model's own topic.
    pub decided_topic: Option<&'a str>,
    /// Drop fragments: points under [`MIN_NOTE_WORDS`] words, actions and
    /// decisions under [`MIN_ACTION_WORDS`] or pointing at nothing ("do
    /// that"). Set for the compact prompt, whose small model writes them.
    pub drop_fragments: bool,
    /// The window's transcript: a note written without a time starts at the
    /// segment it shares the most words with.
    pub segments: &'a [super::scheduler::Segment],
}

/// Applies the rules to one reply's notes in order, so a topic the first
/// note starts is the current topic for the next.
pub struct Refiner<'a> {
    glossary: Option<&'a Glossary>,
    figures: Option<&'a Figures>,
    decided: Option<String>,
    current: Option<String>,
    /// Most recent first.
    topics: Vec<String>,
    drop_fragments: bool,
}

impl<'a> Refiner<'a> {
    pub fn new(context: &NoteContext<'a>) -> Self {
        Self {
            glossary: context.glossary,
            figures: context.figures,
            decided: context.decided_topic.map(str::to_string),
            current: context.current_topic.map(str::to_string),
            topics: context.known_topics.to_vec(),
            drop_fragments: context.drop_fragments,
        }
    }

    /// The note under the rules, or `None` when nothing worth keeping is left.
    pub fn refine(&self, mut entry: ParsedEntry) -> Option<ParsedEntry> {
        let mut text = fix_acronyms(&trim_period(&entry.text));
        if let Some(glossary) = self.glossary {
            text = glossary.canonicalize(&text);
        }

        if let Some(figures) = self.figures {
            let unsupported = figures.unsupported_pieces(&text);
            if !unsupported.is_empty() {
                let pieces: Vec<usize> = unsupported.iter().map(|(piece, _)| *piece).collect();
                text = strip_figures(&text, &pieces)?;
            }
        }

        if entry.kind == EntryKind::Action {
            if let Some((owner, task)) = commitment(&text).or_else(|| their_commitment(&text)) {
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
        text = fit(&text);
        // Judged by the kind the model gave: an ownerless "Email Sarah" is
        // still a short action, not a vague point.
        if self.drop_fragments && is_fragment(&text, entry.kind) {
            return None;
        }

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
        entry.topic = match &self.decided {
            Some(decided) => Some(decided.clone()),
            None => resolve_topic(proposed, self.current.as_deref(), &self.topics),
        };
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

/// Longest note kept, in words (the prompt asks for 6 to 12, never 14).
pub const MAX_NOTE_WORDS: usize = 14;
/// A cut note needs at least this many words.
const MIN_CLAUSE_WORDS: usize = 5;
/// Words a long note may be cut before: what follows them is a detail.
const CLAUSE_WORDS: &[&str] = &[
    "with",
    "and",
    "while",
    "using",
    "across",
    "via",
    "after",
    "before",
    "because",
    "including",
    "but",
    "which",
    "where",
    "despite",
    "through",
    "from",
    "by",
    "for",
    "as",
];
/// Words a cut note never ends on.
const DANGLING: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "with", "for", "in", "on", "at", "by", "from", "as",
    "its", "their", "his", "her", "is", "are", "was", "were", "that", "than", "into",
];

/// "do that", "agreed": nothing a reader can use. Actions and decisions
/// keep two words ("Email Sarah", "Budget approved") unless the second
/// points at nothing.
fn is_fragment(text: &str, kind: EntryKind) -> bool {
    let count = word_count(text);
    match kind {
        EntryKind::Action | EntryKind::Decision => {
            count < MIN_ACTION_WORDS
                || count < MIN_NOTE_WORDS
                    && words(text)
                        .last()
                        .is_some_and(|last| VAGUE_OBJECTS.contains(&last.as_str()))
        }
        _ => count < MIN_NOTE_WORDS,
    }
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// A note of at most `MAX_NOTE_WORDS` words:
/// 1. its leading clauses (split at `;`, ` — ` or `,`) that fit;
/// 2. else the words before the last clause word that leaves at least
///    `MIN_CLAUSE_WORDS` ("… data processing with tens of thousands of GPUs");
/// 3. else the first `MAX_NOTE_WORDS` words without a dangling last word.
fn fit(text: &str) -> String {
    if word_count(text) <= MAX_NOTE_WORDS {
        return text.to_string();
    }
    for separator in ["; ", " — ", " – ", ", "] {
        let mut kept = String::new();
        for clause in text.split(separator) {
            let candidate = if kept.is_empty() {
                clause.to_string()
            } else {
                format!("{kept}{separator}{clause}")
            };
            if word_count(&candidate) > MAX_NOTE_WORDS {
                break;
            }
            kept = candidate;
        }
        if word_count(&kept) >= MIN_CLAUSE_WORDS && word_count(&kept) < word_count(text) {
            return tidy_cut(&kept);
        }
    }
    let pieces: Vec<&str> = text.split_whitespace().collect();
    let lower = words(text);
    // "from X to Y" is one range: never cut inside it.
    let range_from = |i: usize| {
        lower[i] == "from"
            && lower[i + 1..lower.len().min(i + 6)]
                .iter()
                .any(|w| w == "to")
    };
    let cut = (MIN_CLAUSE_WORDS..=MAX_NOTE_WORDS)
        .rev()
        .find(|&i| CLAUSE_WORDS.contains(&lower[i].as_str()) && !range_from(i))
        .unwrap_or(MAX_NOTE_WORDS);
    let mut end = cut;
    // A cut that leaves "from X" without its "to Y" goes back before "from".
    if let Some(open) = (end.saturating_sub(6)..end).find(|&i| {
        range_from(i)
            && lower[i + 1..]
                .iter()
                .position(|w| w == "to")
                .is_some_and(|to| i + 1 + to + 1 >= end)
    }) {
        if open >= MIN_CLAUSE_WORDS {
            end = open;
        }
    }
    while end > MIN_CLAUSE_WORDS && DANGLING.contains(&lower[end - 1].as_str()) {
        end -= 1;
    }
    tidy_cut(&pieces[..end].join(" "))
}

/// A cut note without trailing punctuation or an unclosed parenthesis.
fn tidy_cut(text: &str) -> String {
    let mut text = text.trim().to_string();
    if text.matches('(').count() > text.matches(')').count() {
        if let Some(open) = text.rfind('(') {
            text.truncate(open);
        }
    }
    text.trim_end_matches(|c: char| {
        c == ',' || c == ';' || c == ':' || c == '-' || c == '—' || c.is_whitespace()
    })
    .to_string()
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

/// A note left with fewer words than this after its unsupported figures are
/// taken out is dropped.
const MIN_STRIPPED_WORDS: usize = 4;
/// Words that only qualify the figure after them ("about 40", "$ 5M").
const APPROXIMATORS: &[&str] = &[
    "~",
    "$",
    "about",
    "around",
    "roughly",
    "nearly",
    "almost",
    "over",
    "under",
    "some",
    "approx",
    "approximately",
    "just",
    "only",
    "up",
    "than",
    "more",
    "less",
];
/// Words that only qualify the figure before them ("40 percent").
const FIGURE_UNITS: &[&str] = &["percent", "%", "x"];
/// Prepositions that only lead to the figure after them ("on May 24").
const DATE_PREPOSITIONS: &[&str] = &["on", "by", "at", "until", "before", "after", "since"];
const MONTHS: &[&str] = &[
    "jan",
    "january",
    "feb",
    "february",
    "mar",
    "march",
    "apr",
    "april",
    "may",
    "jun",
    "june",
    "jul",
    "july",
    "aug",
    "august",
    "sep",
    "sept",
    "september",
    "oct",
    "october",
    "nov",
    "november",
    "dec",
    "december",
];

/// `text` without the whitespace pieces at `unsupported` (figures the
/// transcript does not support), or `None` when too little is left.
///
/// 1. When the note has clauses (`;`, ` — `, `,`), the clauses holding an
///    unsupported figure go: "70 people; 4,000 GPUs" → "70 people".
/// 2. Otherwise the figure goes with the words that only qualify it ("about",
///    "$", "percent", a month before a day): "Raised about $5M from Acme" →
///    "Raised from Acme". "from to" left behind becomes "to".
fn strip_figures(text: &str, unsupported: &[usize]) -> Option<String> {
    let pieces: Vec<&str> = text.split_whitespace().collect();
    let bad = |index: usize| unsupported.contains(&index);

    // 1. By clause.
    let mut clauses: Vec<Vec<usize>> = vec![Vec::new()];
    for (index, piece) in pieces.iter().enumerate() {
        if *piece == "—" || *piece == "–" {
            clauses.push(Vec::new());
            continue;
        }
        clauses.last_mut().unwrap().push(index);
        if piece.ends_with([';', ',']) {
            clauses.push(Vec::new());
        }
    }
    clauses.retain(|clause| !clause.is_empty());
    if clauses.len() > 1 {
        let kept: Vec<&Vec<usize>> = clauses
            .iter()
            .filter(|clause| !clause.iter().any(|&i| bad(i)))
            .collect();
        let words: usize = kept.iter().map(|clause| clause.len()).sum();
        if !kept.is_empty() && kept.len() < clauses.len() && words >= MIN_STRIPPED_WORDS {
            let joined = kept
                .iter()
                .map(|clause| {
                    clause
                        .iter()
                        .map(|&i| pieces[i])
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join(" ");
            return Some(tidy_cut(&joined));
        }
    }

    // 2. The figure and its qualifiers.
    let lower: Vec<String> = pieces
        .iter()
        .map(|p| {
            p.trim_matches(|c: char| !c.is_alphanumeric() && c != '~' && c != '$' && c != '%')
                .to_lowercase()
        })
        .collect();
    let mut drop = vec![false; pieces.len()];
    for &index in unsupported {
        if index >= pieces.len() {
            continue;
        }
        drop[index] = true;
        let mut before = index;
        while before > 0 && APPROXIMATORS.contains(&lower[before - 1].as_str()) {
            before -= 1;
            drop[before] = true;
        }
        if before > 0 && MONTHS.contains(&lower[before - 1].as_str()) {
            before -= 1;
            drop[before] = true;
        }
        // "Launch on May 24 for…": the preposition that led to the date.
        if before > 0 && DATE_PREPOSITIONS.contains(&lower[before - 1].as_str()) {
            drop[before - 1] = true;
        }
        if lower
            .get(index + 1)
            .is_some_and(|w| FIGURE_UNITS.contains(&w.as_str()))
        {
            drop[index + 1] = true;
        }
    }
    let mut kept: Vec<&str> = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        if drop[index] {
            continue;
        }
        kept.push(piece);
    }
    // "grew from to 40" → "grew to 40".
    let mut cleaned: Vec<&str> = Vec::new();
    for (index, piece) in kept.iter().enumerate() {
        let next = kept.get(index + 1).map(|p| p.to_lowercase());
        if piece.eq_ignore_ascii_case("from") && next.as_deref() == Some("to") {
            continue;
        }
        cleaned.push(piece);
    }
    let mut words: Vec<&str> = cleaned;
    while words
        .last()
        .is_some_and(|w| DANGLING.contains(&w.trim_end_matches([',', ';']).to_lowercase().as_str()))
    {
        words.pop();
    }
    if words.len() < MIN_STRIPPED_WORDS {
        return None;
    }
    Some(capitalize_first(&tidy_cut(&words.join(" "))))
}

/// Third-person subjects of a commitment by the other side.
const THEIR_SUBJECTS: &[&str] = &[
    "the others",
    "others",
    "they",
    "he",
    "she",
    "the other side",
    "the recruiter",
    "the host",
];

/// "Others will introduce you to the founders", "Priya offers to send the
/// deck", "They'll share the notes": the owner (`Others`, or the name) and
/// the task.
fn their_commitment(text: &str) -> Option<(String, String)> {
    let lower = words(text);
    let contracted_pronoun = matches!(lower.first()?.as_str(), "they'll" | "he'll" | "she'll");
    let (owner, subject_len) = match narrator_len(&lower, THEIR_SUBJECTS) {
        _ if contracted_pronoun => ("Others".to_string(), 1),
        Some(len) => ("Others".to_string(), len),
        None => {
            // One or two capitalized words: a name ("Priya", "Priya Shah").
            let pieces: Vec<&str> = text.split_whitespace().collect();
            let capitalized = |w: &&&str| w.chars().next().is_some_and(char::is_uppercase);
            let len = pieces.iter().take(2).take_while(capitalized).count();
            if len == 0 || FIRST_PERSON.contains(&lower[0].as_str()) {
                return None;
            }
            let name = pieces[..len].join(" ");
            let name = name
                .trim_end_matches("'ll")
                .trim_end_matches("’ll")
                .to_string();
            (name, len)
        }
    };
    let first: &String = lower.get(subject_len - 1)?;
    let contracted = first.ends_with("'ll");
    let after: Vec<&str> = lower[subject_len..].iter().map(String::as_str).collect();
    let skip = if contracted {
        0
    } else {
        match after.as_slice() {
            ["will" | "to", ..] => 1,
            ["is" | "are", "going", "to", ..] => 3,
            ["offer" | "offers" | "offered" | "agree" | "agrees" | "agreed" | "promise"
            | "promises" | "promised" | "plan" | "plans" | "volunteer" | "volunteers"
            | "volunteered", "to", ..] => 2,
            _ => return None,
        }
    };
    let task = after_words(text, subject_len + skip).to_string();
    if word_count(&task) < 2 {
        return None;
    }
    Some((owner, task))
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
        "" | "null"
            | "none"
            | "none yet"
            | "not yet"
            | "unknown"
            | "tbd"
            | "n/a"
            | "general"
            | "misc"
            | "other"
            | "topic"
            | "..."
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

/// A topic runs at least this long before the topic step may end it (the
/// worker does not ask before then), so one aside does not start a chapter.
pub const MIN_TOPIC_MS: i64 = 3 * 60_000;

/// Whether two titles share at least two specific words ("Sequoia
/// Investment Thesis" and "Sequoia's Investment Strategy and Partners"):
/// the same chapter, worded twice.
fn shares_two_specific_words(a: &str, b: &str) -> bool {
    let b_words = topic_words(b);
    let mut shared: Vec<String> = topic_words(a)
        .into_iter()
        .filter(|w| w.len() > 1 && !GENERIC_TOPIC_WORDS.contains(&w.as_str()))
        .filter(|w| b_words.contains(w))
        .collect();
    shared.sort();
    shared.dedup();
    shared.len() >= 2
}

/// The window's topic from the topic step's answer: the current topic when
/// the answer is "same", names a near-duplicate of it, or the current topic
/// is younger than `MIN_TOPIC_MS`; an earlier topic's exact title when the
/// conversation returns to it; otherwise the new title, tidied. `None` when
/// there is no answer (the notes call names the topic then).
///
/// Unlike a topic the notes call proposes, one sharing word does not make it
/// a continuation: "Priya's background" → "Priya's Acme work" is the step
/// saying the subject changed. Two do: that is the same chapter reworded.
pub fn decide_topic(
    decision: Option<&TopicDecision>,
    current: Option<&str>,
    since_ms: Option<i64>,
    window_start_ms: i64,
    known: &[String],
    glossary: Option<&Glossary>,
) -> Option<String> {
    let current = current?;
    let proposed = match decision? {
        TopicDecision::Same => return Some(current.to_string()),
        TopicDecision::New(raw) => raw,
    };
    let Some(proposed) = normalize_topic(proposed).map(|t| fix_acronyms(&t)) else {
        return Some(current.to_string());
    };
    let proposed = match glossary {
        Some(glossary) => glossary.canonicalize(&proposed),
        None => proposed,
    };
    let young = since_ms.is_some_and(|since| window_start_ms - since < MIN_TOPIC_MS);
    if young || same_topic(&proposed, current) || shares_two_specific_words(&proposed, current) {
        return Some(current.to_string());
    }
    if let Some(earlier) = known.iter().find(|known| same_topic(&proposed, known)) {
        return Some(earlier.clone());
    }
    Some(proposed)
}

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
    fn long_notes_are_cut_to_fourteen_words_at_a_clause() {
        let long = "Team grew from ~40 to nearly 100 people; works across the full stack from hardware to data; hiring more";
        assert_eq!(fit(long), "Team grew from ~40 to nearly 100 people");
        let two = "Ten people run ~10 PB a day; own hardware stack; customers overseas and an office opening soon";
        assert_eq!(fit(two), "Ten people run ~10 PB a day; own hardware stack");
        // At a clause word when there are no separators.
        assert_eq!(
            fit("ML infrastructure team handles ten petabytes of daily data processing with tens of thousands of GPUs"),
            "ML infrastructure team handles ten petabytes of daily data processing"
        );
        // A range is never cut at its "to", nor left open after "from".
        assert_eq!(
            fit("Team headcount at the Austin office grew quickly over the year from 30 to 40 people"),
            "Team headcount at the Austin office grew quickly over the year"
        );
        assert_eq!(
            fit("Revenue for the enterprise tier rose sharply from $2M to $5M after the pricing change"),
            "Revenue for the enterprise tier rose sharply from $2M to $5M"
        );
        let tiny = "Yes; the budget for the new office in Austin was approved by finance after a long review";
        assert_eq!(
            fit(tiny),
            "Yes; the budget for the new office in Austin was approved by finance"
        );
        // Never more than 14 words, never ending on a dangling word.
        let no_clause = "word ".repeat(20);
        assert_eq!(word_count(&fit(&no_clause)), MAX_NOTE_WORDS);
        assert_eq!(
            fit("Priya leads the effort of rebuilding the onboarding flow of the app of the year of the decade"),
            "Priya leads the effort of rebuilding the onboarding flow of the app"
        );
        // Short notes stay.
        assert_eq!(
            fit("Budget fine; hiring paused"),
            "Budget fine; hiring paused"
        );
        let refined = refine(note(EntryKind::Point, &"word ".repeat(30), None, None)).unwrap();
        assert!(word_count(&refined.text) <= MAX_NOTE_WORDS);
    }

    #[test]
    fn commitments_by_the_other_side_or_a_named_person_are_kept() {
        let cases = [
            (
                "Others will introduce You to the Mecca AI founders",
                "Others",
                "introduce You to the Mecca AI founders",
            ),
            (
                "They'll share the deck after the call",
                "Others",
                "share the deck after the call",
            ),
            (
                "The recruiter offers to mention the chat to the founders",
                "Others",
                "mention the chat to the founders",
            ),
            (
                "Priya agreed to send the pricing model",
                "Priya",
                "Priya — send the pricing model",
            ),
        ];
        for (input, owner, text) in cases {
            let refined = refine(note(EntryKind::Action, input, None, None)).unwrap();
            assert_eq!(refined.kind, EntryKind::Action, "{input}");
            assert_eq!(refined.speaker.as_deref(), Some(owner), "{input}");
            assert_eq!(refined.text, text, "{input}");
        }
        // The two the prompt shows as good examples survive as written.
        for (owner, task) in [
            ("You", "connect with the Mecca AI founders"),
            ("You", "connect with the robotics lead on LinkedIn"),
        ] {
            let refined = refine(note(EntryKind::Action, task, Some(owner), None)).unwrap();
            assert_eq!(
                (refined.kind, refined.text.as_str()),
                (EntryKind::Action, task)
            );
        }
        // Narration stays a point, as do suggestions nobody took on.
        for narrated in [
            "Others suggest connecting with the founders",
            "Others discuss reaching out to founders",
        ] {
            let refined = refine(note(EntryKind::Action, narrated, Some("Others"), None)).unwrap();
            assert_eq!(refined.kind, EntryKind::Point, "{narrated}");
        }
    }

    #[test]
    fn an_unsupported_figure_is_taken_out_of_the_note() {
        let figures = Figures::of(
            "they have about seventy people and ten petabytes a day; the raise was big",
        );
        let context = NoteContext {
            figures: Some(&figures),
            ..Default::default()
        };
        let refiner = Refiner::new(&context);
        let text = |input: &str| {
            refiner
                .refine(note(EntryKind::Point, input, None, None))
                .map(|entry| entry.text)
        };
        let cases: &[(&str, Option<&str>)] = &[
            // Supported: unchanged.
            (
                "70 people handle 10 PB a day",
                Some("70 people handle 10 PB a day"),
            ),
            // The clause with the figure goes.
            (
                "70 people handle 10 PB a day; 4,000 GPUs in use",
                Some("70 people handle 10 PB a day"),
            ),
            // The figure and its qualifiers go.
            (
                "Raised about $5M from Acme and Globex",
                Some("Raised from Acme and Globex"),
            ),
            ("Team of 70 runs 4,000 GPUs", Some("Team of 70 runs GPUs")),
            (
                "Usage up 40 percent among 70 people",
                Some("Usage among 70 people"),
            ),
            (
                "Launch on May 24 for the 70 people",
                Some("Launch for the 70 people"),
            ),
            ("Grew from 30 to 70 people", Some("Grew to 70 people")),
            // Little more than the figure: dropped.
            ("4,000 GPUs", None),
            ("About 4,000 GPUs in total", None),
        ];
        for (input, expected) in cases {
            assert_eq!(text(input).as_deref(), *expected, "{input}");
        }
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
    fn the_topic_step_decides_the_window_topic() {
        let known = vec!["Priya's Background".to_string(), "Launch Plan".to_string()];
        let decide = |decision: TopicDecision, since: i64| {
            decide_topic(
                Some(&decision),
                Some("Priya's Background"),
                Some(since),
                10 * 60_000,
                &known,
                None,
            )
        };
        let new = |t: &str| TopicDecision::New(t.to_string());
        assert_eq!(
            decide(TopicDecision::Same, 0).as_deref(),
            Some("Priya's Background")
        );
        // A sharing word is still a new subject when the step says so.
        assert_eq!(
            decide(new("Priya's Acme work"), 0).as_deref(),
            Some("Priya's Acme Work")
        );
        // A near-duplicate of the current title, or a young topic, stays.
        assert_eq!(
            decide(new("priya background"), 0).as_deref(),
            Some("Priya's Background")
        );
        assert_eq!(
            decide(new("Acme data platform"), 9 * 60_000).as_deref(),
            Some("Priya's Background")
        );
        // The same chapter reworded keeps its title.
        let sequoia = decide_topic(
            Some(&new("Sequoia's Investment Strategy and Partners")),
            Some("Sequoia Investment Thesis"),
            Some(0),
            10 * 60_000,
            &known,
            None,
        );
        assert_eq!(sequoia.as_deref(), Some("Sequoia Investment Thesis"));
        // Returning to an earlier topic reuses its title.
        assert_eq!(
            decide(new("the launch plan"), 0).as_deref(),
            Some("Launch Plan")
        );
        // No answer, or no current topic: the notes call decides.
        assert_eq!(
            decide_topic(None, Some("Launch Plan"), None, 0, &known, None),
            None
        );
        assert_eq!(
            decide_topic(Some(&TopicDecision::Same), None, None, 0, &known, None),
            None
        );

        // Every note of the window takes the decided topic.
        let context = NoteContext {
            current_topic: Some("Priya's Background"),
            decided_topic: Some("Acme Data Platform"),
            ..Default::default()
        };
        let refined = Refiner::new(&context)
            .refine(note(
                EntryKind::Point,
                "Platform ingests ~10 TB a day",
                None,
                Some("Priya's background"),
            ))
            .unwrap();
        assert_eq!(refined.topic.as_deref(), Some("Acme Data Platform"));
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
