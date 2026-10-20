//! Fine-tuned live notes: Gemma 4 E2B with the live notes LoRA adapter, which
//! was trained on its own compact format. Each pause-aligned window it reads
//! the notes so far (with ids, the user's marked `[you]`), the topic, the
//! last notes of closed topics and the window's transcript, and answers with
//! the changes only: a topic line, new notes, sub-bullets, edits and drops,
//! or "." for nothing.
//!
//! A port of the training side (~/Developer/minutes-distill), which is the
//! source of truth: `render_training.py` (the system prompt and the user
//! prompt), `compact_ops.py` (the answer), `validate_labels.py` (`Living`:
//! the notes, the user's events, the rules an op must pass, the duplicate
//! checks) and `train/closed_loop.py` (the loop: a sub-bullet that cannot
//! attach becomes its own note). `fixtures/finetuned/` holds what that Python
//! produced; the tests below match it byte for byte.
//!
//! The worker maps the notes onto timeline entries (`Session`): an add or a
//! sub-bullet is a new entry, an edit changes one, a drop removes one. Only
//! the model's own (`[ai]`) notes become entries; the user's lines stay in
//! their notes document and reach the model as `[you]` notes.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

use super::prompt::format_clock;
use super::scheduler::{Segment, Window};
use super::worker::TimelineEntry;

/// The system prompt the adapter is trained with (`render_training.py`
/// `SYSTEM_PROMPT`).
pub const SYSTEM_PROMPT: &str = "Live meeting notetaker. Given NEW TRANSCRIPT (CONTEXT is already covered), the topic and CURRENT NOTES, output only the changes, one per line:
T=<topic> new topic (2-5 words; only if the subject really changed, >=90 s after the last)
+<k> <start>[-<end>]|<note> new note; k = p point, d decision, a action (owner after the time)
><id> <k> <start>[-<end>]|<note> sub-bullet: a detail of note <id> itself (max 3, never an action)
~<id>|<note> rewrite an [ai] note
-<id> drop an [ai] note
. nothing to note (normal)
New notes take NEXT ID in order and carry no id.
Style: 6-14 word headlines, facts and numbers first, firm wording, \";\" merges facts. No narration, process, small talk or unsaid figures. Edit, don't re-add. Lists: parent plus children. Decision = explicit agreement; action = a task taken on. Never edit/drop [you] notes; expand their shorthand with sub-bullets.";

/// "." (nothing to note) is answered without generating when the model gives
/// it at least this probability as the first token; below it, "." is banned
/// as the first token and the model writes its changes (`closed_loop.py`).
pub const GATE_TOKEN: &str = ".";
pub const GATE_THRESHOLD: f32 = 0.9;
/// Output cap: the longest training answer is under 100 tokens.
pub const MAX_OUTPUT_TOKENS: u32 = 300;
/// Context per request: the longest training example is under 1,900 tokens.
pub const CONTEXT_SIZE: u32 = 4096;
/// The previous window's tail shown as context (the training windows').
pub const CONTEXT_MS: i64 = 20_000;
/// Last notes of closed topics shown in the prompt.
pub const CLOSED_NOTES_IN_PROMPT: usize = 4;
/// Earlier topics' notes a new note must not repeat (the app's repeat check).
const CLOSED_REPEAT_HISTORY: usize = 8;

// ---------------------------------------------------------------------------
// The answer (compact_ops.py)
// ---------------------------------------------------------------------------

/// A note's id: the model's notes are numbered, the user's are `u1`, `u2`…
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteId {
    Ai(u32),
    User(u32),
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ai(id) => write!(f, "{id}"),
            Self::User(id) => write!(f, "u{id}"),
        }
    }
}

impl NoteId {
    /// `7` or `u2`. An id too large to be real can never match a note.
    fn parse(text: &str) -> Self {
        let number = |digits: &str| digits.parse().unwrap_or(u32::MAX);
        match text.strip_prefix('u') {
            Some(digits) => Self::User(number(digits)),
            None => Self::Ai(number(text)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Point,
    Decision,
    Action,
}

impl Kind {
    fn from_code(code: &str) -> Self {
        match code {
            "d" => Self::Decision,
            "a" => Self::Action,
            _ => Self::Point,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Point => "point",
            Self::Decision => "decision",
            Self::Action => "action",
        }
    }
}

/// A new note (`+`) or sub-bullet (`>`). Times are as written (`mm:ss`).
#[derive(Debug, Clone, PartialEq)]
pub struct NewNote {
    pub id: u32,
    pub start: String,
    pub end: String,
    pub kind: Kind,
    pub owner: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Add(NewNote),
    Child {
        parent: NoteId,
        note: NewNote,
    },
    /// Fields other than the text change only when given.
    Edit {
        id: NoteId,
        text: String,
        kind: Option<Kind>,
        end: Option<String>,
        owner: Option<String>,
    },
    Drop(NoteId),
}

/// A decoded answer: the new topic, if any, and the ops in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decoded {
    pub topic: Option<String>,
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OpsError(String);

const CLOCK: &str = r"\d{2,3}:\d{2}";
const ID: &str = r"(?:\d+|u\d+)";

static RE_ADD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^\+([pda]) ({CLOCK})(?:-({CLOCK}))?(?: (.+?))?\|(.*)$"
    ))
    .unwrap()
});
static RE_CHILD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^>({ID}) ([pda]) ({CLOCK})(?:-({CLOCK}))?(?: (.+?))?\|(.*)$"
    ))
    .unwrap()
});
static RE_EDIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^~({ID})((?: k=[pda])?)((?: @{CLOCK})?)((?: o=[^|]+)?)\|(.*)$"
    ))
    .unwrap()
});
static RE_DROP: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^-({ID})$")).unwrap());

fn new_note(
    code: &str,
    start: &str,
    end: Option<&str>,
    owner: Option<&str>,
    text: &str,
    id: u32,
) -> Result<NewNote, OpsError> {
    let kind = Kind::from_code(code);
    if kind != Kind::Action && owner.is_some() {
        return Err(OpsError("owner on non-action".into()));
    }
    Ok(NewNote {
        id,
        start: start.to_string(),
        end: end.unwrap_or(start).to_string(),
        kind,
        owner: owner.map(str::to_string),
        text: text.to_string(),
    })
}

fn group<'t>(captures: &regex::Captures<'t>, i: usize) -> Option<&'t str> {
    captures.get(i).map(|m| m.as_str())
}

/// Parses an answer; new notes take ids from `next_id` in order. Strict:
/// any line that does not parse fails the whole answer (`compact_ops.decode`).
pub fn decode(text: &str, next_id: u32) -> Result<Decoded, OpsError> {
    let mut decoded = Decoded::default();
    let mut id = next_id;
    let trimmed = text.trim();
    let mut lines: Vec<&str> = if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('\n').collect()
    };
    if lines == ["."] {
        lines.clear();
    }
    for (n, line) in lines.iter().enumerate() {
        if let Some(topic) = line.strip_prefix("T=") {
            if n != 0 || topic.trim().is_empty() {
                return Err(OpsError(format!(
                    "topic line must be first and non-empty: {line:?}"
                )));
            }
            decoded.topic = Some(topic.trim().to_string());
            continue;
        }
        if let Some(c) = RE_ADD.captures(line) {
            let note = new_note(&c[1], &c[2], group(&c, 3), group(&c, 4), &c[5], id)?;
            decoded.ops.push(Op::Add(note));
            id += 1;
        } else if let Some(c) = RE_CHILD.captures(line) {
            let note = new_note(&c[2], &c[3], group(&c, 4), group(&c, 5), &c[6], id)?;
            decoded.ops.push(Op::Child {
                parent: NoteId::parse(&c[1]),
                note,
            });
            id += 1;
        } else if let Some(c) = RE_EDIT.captures(line) {
            let field = |i: usize, prefix: &str| {
                group(&c, i).filter(|value| !value.is_empty()).map(|value| {
                    let value = value.trim();
                    value.strip_prefix(prefix).unwrap_or(value).to_string()
                })
            };
            decoded.ops.push(Op::Edit {
                id: NoteId::parse(&c[1]),
                kind: field(2, "k=").map(|code| Kind::from_code(&code)),
                end: field(3, "@"),
                owner: field(4, "o="),
                text: c[5].to_string(),
            });
        } else if let Some(c) = RE_DROP.captures(line) {
            decoded.ops.push(Op::Drop(NoteId::parse(&c[1])));
        } else {
            return Err(OpsError(format!("unparseable line {n}: {line:?}")));
        }
    }
    Ok(decoded)
}

// ---------------------------------------------------------------------------
// The notes (validate_labels.py Living)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Ai,
    You,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub parent: Option<NoteId>,
    pub text: String,
    pub kind: Kind,
    pub owner: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub dropped: bool,
    /// Whose it is now: an `[ai]` note the user edits becomes `[you]`.
    pub mark: Mark,
    /// Who wrote it first.
    pub origin: Mark,
}

/// What the user did in their notes (`simulate_user.py`'s events).
#[derive(Debug, Clone, PartialEq)]
pub enum UserEvent {
    /// A new line of theirs, `u<id>`, typed at `t` (`mm:ss`).
    Add {
        id: u32,
        t: Option<String>,
        text: String,
    },
    /// They rewrote a note: it is theirs from now on.
    Edit { id: NoteId, text: String },
    /// They deleted one of the model's notes (and so its sub-bullets).
    Delete { id: NoteId },
}

/// Sub-bullets per note.
pub const MAX_CHILDREN: usize = 3;

static TASKY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^[\w ]{1,30}? (to|will|should|needs to|has to|is to) (do|write|prepare|look|research|investigate|build|draft|send|create|design|check|find|work|make|come|review|present|bring|prototype|develop|finish|share|circulate|update|ask|contact|test|evaluate|think)\b",
    )
    .unwrap()
});

/// The meeting's notes: every topic's, in the order they were made. `cur`
/// holds the open topic's; a closed topic's notes are final.
#[derive(Debug, Clone, Default)]
pub struct Living {
    notes: HashMap<NoteId, Note>,
    pub cur: HashSet<NoteId>,
    pub maxid: u32,
    pub order: Vec<NoteId>,
    pub topic: Option<String>,
}

impl Living {
    pub fn note(&self, id: NoteId) -> Option<&Note> {
        self.notes.get(&id)
    }

    /// The topic closes: its notes are final.
    pub fn new_topic(&mut self) {
        self.cur.clear();
    }

    /// Applies a user event; returns whether it changed the notes. Events
    /// that cannot apply (no topic yet, an unknown or closed target) are
    /// ignored.
    pub fn apply_event(&mut self, event: &UserEvent) -> bool {
        match event {
            UserEvent::Add { id, t, text } => {
                let id = NoteId::User(*id);
                if self.topic.is_none() || self.notes.contains_key(&id) {
                    return false;
                }
                self.notes.insert(
                    id,
                    Note {
                        parent: None,
                        text: text.clone(),
                        kind: Kind::Point,
                        owner: None,
                        start: t.clone(),
                        end: t.clone(),
                        dropped: false,
                        mark: Mark::You,
                        origin: Mark::You,
                    },
                );
                self.cur.insert(id);
                self.order.push(id);
                true
            }
            UserEvent::Edit { id, text } => match self.open_note(*id) {
                Some(note) => {
                    note.text = text.clone();
                    note.mark = Mark::You;
                    true
                }
                None => false,
            },
            UserEvent::Delete { id } => match self.open_note(*id) {
                Some(note) if note.mark == Mark::Ai => {
                    note.dropped = true;
                    self.drop_children(*id);
                    true
                }
                _ => false,
            },
        }
    }

    /// A note of the open topic that is not dropped.
    fn open_note(&mut self, id: NoteId) -> Option<&mut Note> {
        if !self.cur.contains(&id) {
            return None;
        }
        self.notes.get_mut(&id).filter(|note| !note.dropped)
    }

    fn drop_children(&mut self, id: NoteId) {
        for note in self.notes.values_mut() {
            if note.parent == Some(id) {
                note.dropped = true;
            }
        }
    }

    /// A note's sub-bullets that are not dropped, in order.
    pub fn children(&self, id: NoteId) -> Vec<NoteId> {
        self.order
            .iter()
            .copied()
            .filter(|j| {
                let note = &self.notes[j];
                note.parent == Some(id) && !note.dropped
            })
            .collect()
    }

    /// The open topic's notes as the model sees them: ids, `[you]`/`[ai]`,
    /// sub-bullets indented under their note.
    pub fn render(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for &i in &self.order {
            let note = &self.notes[&i];
            if !self.cur.contains(&i) || note.dropped || note.parent.is_some() {
                continue;
            }
            for j in std::iter::once(i).chain(self.children(i)) {
                let c = &self.notes[&j];
                let tag = match (c.mark, c.origin) {
                    (Mark::You, Mark::Ai) => "[you] (you edited an ai note: now theirs)",
                    (Mark::You, Mark::You) => "[you]",
                    (Mark::Ai, _) => "[ai]",
                };
                let kind = match c.kind {
                    Kind::Point => String::new(),
                    kind => match c.owner.as_deref().filter(|o| !o.is_empty()) {
                        Some(owner) => format!(" ({}, {owner})", kind.as_str()),
                        None => format!(" ({})", kind.as_str()),
                    },
                };
                let indent = if c.parent.is_some() { "  " } else { "" };
                lines.push(format!("{indent}{j} {tag} {}{kind}", c.text));
            }
        }
        lines
    }

    /// Applies an op when it is valid; returns why not otherwise (empty when
    /// applied). The reasons are the validator's.
    pub fn apply(&mut self, op: &Op) -> Vec<String> {
        match op {
            Op::Add(note) => self.insert(note, None),
            Op::Child { parent, note } => self.insert(note, Some(*parent)),
            Op::Drop(id) => {
                if let Some(reason) = self.target_problem(*id, "drop") {
                    return vec![reason];
                }
                self.notes.get_mut(id).expect("checked").dropped = true;
                self.drop_children(*id);
                Vec::new()
            }
            Op::Edit {
                id,
                text,
                kind,
                end,
                owner,
            } => {
                if let Some(reason) = self.target_problem(*id, "edit") {
                    return vec![reason];
                }
                let target = self.notes.get_mut(id).expect("checked");
                if target.parent.is_some() && *kind == Some(Kind::Action) {
                    return vec!["action_must_be_top_level".into()];
                }
                let new_kind = kind.unwrap_or(target.kind);
                let new_owner = match (owner, kind) {
                    (Some(owner), _) => Some(owner.clone()),
                    // Leaving action clears the owner unless one is given.
                    (None, Some(kind)) if *kind != Kind::Action => None,
                    (None, _) => target.owner.clone(),
                };
                let new_end = end.clone().or_else(|| target.end.clone());
                if *text == target.text
                    && new_kind == target.kind
                    && new_owner == target.owner
                    && new_end == target.end
                {
                    return vec!["edit_changes_nothing".into()];
                }
                target.text = text.clone();
                target.kind = new_kind;
                target.owner = new_owner;
                target.end = new_end;
                Vec::new()
            }
        }
    }

    fn target_problem(&self, id: NoteId, verb: &str) -> Option<String> {
        let Some(target) = self.notes.get(&id) else {
            return Some(format!("target_unknown:{id}"));
        };
        if !self.cur.contains(&id) {
            Some(format!("target_in_closed_topic:{id}"))
        } else if target.dropped {
            Some(format!("target_dropped:{id}"))
        } else if target.mark == Mark::You {
            Some(format!(
                "{verb}_of_user_note:{id}_notes_marked_you_are_read_only"
            ))
        } else {
            None
        }
    }

    fn insert(&mut self, note: &NewNote, parent: Option<NoteId>) -> Vec<String> {
        let id = NoteId::Ai(note.id);
        let mut reasons = Vec::new();
        if self.notes.contains_key(&id) {
            reasons.push(format!("id_not_unique:{id}"));
        } else if note.id <= self.maxid {
            reasons.push(format!("id_not_increasing:{id}<={}", self.maxid));
        }
        if let Some(parent) = parent {
            match self.notes.get(&parent) {
                None => reasons.push(format!("parent_unknown:{parent}")),
                Some(_) if !self.cur.contains(&parent) => {
                    reasons.push(format!("parent_in_closed_topic:{parent}"))
                }
                Some(p) if p.parent.is_some() => {
                    reasons.push(format!("nested_child:{parent}_is_a_child"))
                }
                Some(p) if p.dropped => reasons.push(format!("parent_dropped:{parent}")),
                Some(_) if self.children(parent).len() >= MAX_CHILDREN => reasons.push(format!(
                    "too_many_children:{parent}_has_{MAX_CHILDREN}_start_a_new_note_or_merge"
                )),
                Some(_) => {}
            }
            if note.kind == Kind::Action {
                reasons.push("action_must_be_top_level".into());
            } else if TASKY.is_match(note.text.trim()) {
                reasons.push("assignment_as_child_use_action_op".into());
            }
        }
        if !reasons.is_empty() {
            return reasons;
        }
        self.notes.insert(
            id,
            Note {
                parent,
                text: note.text.clone(),
                kind: note.kind,
                owner: note.owner.clone(),
                start: Some(note.start.clone()),
                end: Some(note.end.clone()),
                dropped: false,
                mark: Mark::Ai,
                origin: Mark::Ai,
            },
        );
        self.cur.insert(id);
        self.order.push(id);
        self.maxid = self.maxid.max(note.id);
        Vec::new()
    }

    /// The open topic's notes that are not dropped, in order.
    fn open_texts(&self) -> Vec<String> {
        self.order
            .iter()
            .filter(|i| self.cur.contains(i) && !self.notes[i].dropped)
            .map(|i| self.notes[i].text.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Duplicates (validate_labels.py dup_checks)
// ---------------------------------------------------------------------------

const DUP_STOP: &[&str] = &[
    "the", "a", "an", "of", "to", "and", "in", "on", "for", "is", "are", "was", "were", "be", "it",
    "that", "this", "we", "i", "you", "they", "with", "as", "at", "by", "from", "or", "but", "so",
    "if", "then",
];

static DIGIT_LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d)([a-z])").unwrap());
static LETTER_DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z])(\d)").unwrap());
static DUP_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z]+|\d+(?:\.\d+)?").unwrap());

fn is_digits(word: &str) -> bool {
    !word.is_empty() && word.chars().all(char::is_numeric)
}

/// Words for duplicate detection: letters and digits split apart, currency
/// words unified, crudely stemmed (`dtoks`).
fn dup_words(text: &str) -> HashSet<String> {
    let lower = text
        .to_lowercase()
        .replace('€', " eur ")
        .replace('%', " percent ");
    let spaced = DIGIT_LETTER.replace_all(&lower, "$1 $2");
    let spaced = LETTER_DIGIT.replace_all(&spaced, "$1 $2");
    let mut out = HashSet::new();
    for word in DUP_WORD.find_iter(&spaced).map(|m| m.as_str()) {
        if DUP_STOP.contains(&word) || (word.chars().count() < 2 && !is_digits(word)) {
            continue;
        }
        let mut word = match word {
            "euros" | "euro" => "eur",
            "dollars" | "dollar" => "usd",
            "pounds" | "pound" => "gbp",
            other => other,
        }
        .to_string();
        if !is_digits(&word) {
            for suffix in ["ing", "ed", "es", "s", "e"] {
                if word.ends_with(suffix) && word.len() - suffix.len() >= 3 {
                    word.truncate(word.len() - suffix.len());
                    break;
                }
            }
        }
        out.insert(word);
    }
    out
}

/// Whether `text` restates (almost) everything the user's note says.
pub fn covers_user(user_text: &str, text: &str) -> bool {
    let (user, words) = (dup_words(user_text), dup_words(text));
    if user.len() < 2 {
        return false;
    }
    let shared = user.intersection(&words).count();
    shared >= 2 && shared as f64 >= 0.6 * user.len() as f64
}

/// Whether two notes say the same thing (an add that should have been an edit).
pub fn near_duplicate(a_text: &str, b_text: &str) -> bool {
    let (a, b) = (dup_words(a_text), dup_words(b_text));
    let shared: Vec<&String> = a.intersection(&b).collect();
    if shared.len() < 2 {
        return false;
    }
    let enough = shared.len() as f64 >= 0.6 * a.len().min(b.len()) as f64;
    if shared.len() >= 3 && enough {
        return true;
    }
    let figure = shared
        .iter()
        .any(|w| w.chars().next().is_some_and(char::is_numeric));
    figure && enough
}

/// Why an op duplicates the open topic's notes, judged before it is
/// applied: it restates a `[you]` note (the user wrote it already), or an
/// add says what an `[ai]` note says (it should have been an edit).
pub fn duplicate_reasons(op: &Op, living: &Living) -> Vec<String> {
    let (me, parent, text, old_text) = match op {
        Op::Add(note) => (NoteId::Ai(note.id), None, &note.text, None),
        Op::Child { parent, note } => (NoteId::Ai(note.id), Some(*parent), &note.text, None),
        Op::Edit { id, text, .. } => match living.note(*id) {
            Some(target) => (*id, target.parent, text, Some(target.text.as_str())),
            None => return Vec::new(),
        },
        Op::Drop(_) => return Vec::new(),
    };
    let mut reasons = Vec::new();
    for &j in &living.order {
        let c = &living.notes[&j];
        if j == me || !living.cur.contains(&j) || c.dropped || Some(j) == parent {
            continue;
        }
        if c.mark == Mark::You {
            // An edit may keep words the note had before the user wrote theirs.
            let kept = old_text.is_some_and(|old| covers_user(&c.text, old));
            if covers_user(&c.text, text) && !kept {
                reasons.push(format!("duplicates_user_note:{j}"));
            }
        } else if matches!(op, Op::Add(_)) && near_duplicate(text, &c.text) {
            reasons.push(format!("near_duplicate_add_of:{j}_edit_it_instead"));
            break;
        }
    }
    reasons
}

/// A sub-bullet rejected only for these becomes its own note (the closed
/// loop's repair).
const REPAIRABLE: &[&str] = &[
    "too_many_children",
    "parent_unknown",
    "nested_child",
    "parent_in_closed_topic",
];

/// What happened to one op of an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The op as applied (a repaired sub-bullet is an add).
    pub op: Op,
    /// Why it was rejected: duplicate reasons, else the notes' rules.
    pub rejected: Vec<String>,
    /// A sub-bullet that became its own note, and why it could not attach.
    pub repaired_from: Vec<String>,
}

/// Applies one op as the runtime does: the duplicate filter first, then the
/// notes' rules, and a sub-bullet that cannot attach becomes its own note.
pub fn apply_op(living: &mut Living, op: Op) -> Applied {
    let duplicate = duplicate_reasons(&op, living);
    if !duplicate.is_empty() {
        return Applied {
            op,
            rejected: duplicate,
            repaired_from: Vec::new(),
        };
    }
    let reasons = living.apply(&op);
    let repairable = !reasons.is_empty()
        && reasons
            .iter()
            .all(|r| REPAIRABLE.contains(&r.split(':').next().unwrap_or_default()));
    match op {
        Op::Child { note, .. } if repairable => {
            let op = Op::Add(note);
            let duplicate = duplicate_reasons(&op, living);
            let rejected = if duplicate.is_empty() {
                living.apply(&op)
            } else {
                duplicate
            };
            Applied {
                op,
                rejected,
                repaired_from: reasons,
            }
        }
        op => Applied {
            op,
            rejected: reasons,
            repaired_from: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------
// The prompt (render_training.py)
// ---------------------------------------------------------------------------

/// Capitalised words the glossary never lists.
const GLOSSARY_STOP: &[&str] = &[
    "The", "A", "An", "I", "We", "You", "They", "He", "She", "It", "This", "That", "These",
    "Those", "And", "But", "So", "Or", "If", "In", "On", "At", "To", "Of", "For", "With", "As",
    "Is", "Are", "Was", "Were", "Be", "Not", "No", "Yes", "One", "Two", "Three", "Four", "Five",
    "Six", "Seven", "Eight", "Nine", "Ten", "First", "Second", "Third", "Teletext", "TV",
];
const GLOSSARY_LIMIT: usize = 12;

static GLOSSARY_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z][A-Za-z0-9'’-]*").unwrap());
static POSSESSIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"['’]s$").unwrap());

/// Spellings to keep: names, acronyms and product words in the notes so far
/// (capitalised mid-sentence, or all capitals).
pub fn glossary<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for text in texts {
        for (i, m) in GLOSSARY_WORD.find_iter(text).enumerate() {
            let word = POSSESSIVE.replace(m.as_str(), "").to_string();
            let all_caps =
                word.chars().any(char::is_alphabetic) && !word.chars().any(char::is_lowercase);
            let capital = word.chars().next().is_some_and(char::is_uppercase)
                && (i > 0 || all_caps)
                && word.chars().count() > 1;
            if capital && !GLOSSARY_STOP.contains(&word.as_str()) && !seen.contains(&word) {
                seen.push(word);
            }
        }
    }
    seen.truncate(GLOSSARY_LIMIT);
    seen
}

/// A window's text as the prompt shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowText {
    pub start: String,
    pub end: String,
    pub max_entries: usize,
    pub transcript: String,
    /// The previous window's tail; empty for none.
    pub context: String,
}

fn transcript_lines(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|s| {
            format!(
                "[{}] {}: {}",
                format_clock(s.start_ms),
                s.speaker.label(),
                s.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl WindowText {
    /// `previous` is the window before it: its last `CONTEXT_MS` is the context.
    pub fn of(window: &Window, previous: &[Segment]) -> Self {
        let context = super::prompt::carryover_within(previous, window.start_ms, CONTEXT_MS);
        Self {
            start: format_clock(window.start_ms),
            end: format_clock(window.end_ms),
            max_entries: window.max_entries,
            transcript: transcript_lines(&window.segments),
            context: transcript_lines(&context),
        }
    }
}

/// The user turn of one window (`render_user`): the notes as they stand
/// after the user's events of the window, before the window's own changes.
pub fn render_user(
    window: &WindowText,
    living: &Living,
    topic: Option<&str>,
    topic_since: Option<&str>,
    closed_notes: &[String],
) -> String {
    let mut lines = vec![
        format!(
            "WINDOW: {} to {}   BUDGET: at most {} new notes",
            window.start, window.end, window.max_entries
        ),
        match (topic, topic_since) {
            (Some(topic), Some(since)) => format!("TOPIC: {topic} (since {since})"),
            (Some(topic), None) => format!("TOPIC: {topic} (since None)"),
            (None, _) => "TOPIC: none yet".to_string(),
        },
        format!("NEXT ID: {}", living.maxid + 1),
    ];
    let texts: Vec<&str> = living
        .order
        .iter()
        .filter(|i| !living.notes[i].dropped)
        .map(|i| living.notes[i].text.as_str())
        .collect();
    let terms = glossary(texts);
    if !terms.is_empty() {
        lines.push(format!("GLOSSARY: {}", terms.join(", ")));
    }
    if !closed_notes.is_empty() {
        lines.push(format!(
            "EARLIER TOPICS, last notes: {}",
            closed_notes.join(" | ")
        ));
    }
    lines.push("CURRENT NOTES:".into());
    let notes = living.render();
    if notes.is_empty() {
        lines.push("  (none)".into());
    } else {
        lines.extend(notes);
    }
    if window.context.is_empty() {
        lines.push("CONTEXT: (none)".into());
    } else {
        lines.push("CONTEXT (already covered, never note it):".into());
        lines.push(window.context.clone());
    }
    lines.push("NEW TRANSCRIPT:".into());
    lines.push(window.transcript.clone());
    lines.join("\n")
}

/// The system and user turns as the chat template takes them: Gemma 4's
/// template (Hugging Face `apply_chat_template`) trims both, and the app's
/// `gemma4` template does not.
pub fn turns(user: &str) -> (&'static str, String) {
    (SYSTEM_PROMPT.trim(), user.trim().to_string())
}

// ---------------------------------------------------------------------------
// The session: the closed loop, mapped onto the app's entries
// ---------------------------------------------------------------------------

/// One line of the user's notes document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocLine {
    /// The editor block's id.
    pub block_id: String,
    /// When it was typed (audio time), when known.
    pub ms: Option<i64>,
    pub text: String,
    /// The live notes entry the AI wrote this line for; `None` for the user's.
    pub ai_entry: Option<String>,
}

/// A change to the timeline's entries.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryChange {
    Add(TimelineEntry),
    /// An entry's text, kind, owner or end changed.
    Update(TimelineEntry),
    Remove(String),
}

/// What the notes of a note were before an answer, to tell what changed.
type NoteState = (String, Kind, Option<String>, Option<String>, bool);

/// The fine-tuned live notes of one recording.
#[derive(Debug, Default)]
pub struct Session {
    pub living: Living,
    /// When the current topic started (`mm:ss` of its first window).
    since: Option<String>,
    /// Notes of closed topics, in order.
    closed: Vec<String>,
    /// The model's notes that are entries.
    entries: HashMap<NoteId, TimelineEntry>,
    /// Editor blocks the AI wrote, by block id: the entry each shows.
    ai_blocks: HashMap<String, String>,
    /// The user's lines (and AI lines they took over): the note and its text.
    user_blocks: HashMap<String, (NoteId, String)>,
    next_user: u32,
}

/// `mm:ss` (or `mmm:ss`) as milliseconds.
fn clock_ms(clock: &str) -> Option<i64> {
    let (minutes, seconds) = clock.split_once(':')?;
    Some((minutes.parse::<i64>().ok()? * 60 + seconds.parse::<i64>().ok()?) * 1000)
}

/// An AI line as the notes document shows it, without what the editor adds
/// in front ("Decision: ", "Action: Owner — ").
fn without_ai_prefix(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("Decision: ") {
        return rest;
    }
    if let Some(rest) = text.strip_prefix("Action: ") {
        return rest.split_once(" — ").map_or(rest, |(_, task)| task);
    }
    text
}

impl Session {
    pub fn topic(&self) -> Option<&str> {
        self.living.topic.as_deref()
    }

    /// Reads the user's notes document: new lines are `[you]` notes, lines
    /// they changed are edits, AI lines they rewrote become theirs, and AI
    /// lines they deleted are dropped. Returns the entries of deleted lines.
    pub fn sync_document(&mut self, lines: &[DocLine]) -> Vec<String> {
        let by_block: HashMap<&str, &DocLine> =
            lines.iter().map(|l| (l.block_id.as_str(), l)).collect();
        let mut removed = Vec::new();
        let mut taken_over = HashSet::new();
        // The AI's lines the user changed since the last read.
        let mut known: Vec<(String, String)> = self
            .ai_blocks
            .iter()
            .map(|(b, e)| (b.clone(), e.clone()))
            .collect();
        known.sort();
        for (block, entry) in known {
            let line = by_block.get(block.as_str());
            if line.is_some_and(|l| l.ai_entry.is_some()) {
                continue;
            }
            self.ai_blocks.remove(&block);
            let Some(id) = self.note_of_entry(&entry) else {
                continue;
            };
            match line {
                Some(line) => {
                    let text = without_ai_prefix(line.text.trim()).to_string();
                    self.living.apply_event(&UserEvent::Edit {
                        id,
                        text: text.clone(),
                    });
                    self.user_blocks.insert(block.clone(), (id, text));
                    taken_over.insert(block);
                }
                None => {
                    if self.living.apply_event(&UserEvent::Delete { id }) {
                        removed.extend(self.forget_dropped());
                    }
                }
            }
        }
        for line in lines {
            if let Some(entry) = &line.ai_entry {
                self.ai_blocks.insert(line.block_id.clone(), entry.clone());
                continue;
            }
            if taken_over.contains(&line.block_id) || line.text.trim().is_empty() {
                continue;
            }
            let text = line.text.trim().to_string();
            match self.user_blocks.get(&line.block_id) {
                Some((id, known)) if *known != text => {
                    let id = *id;
                    self.living.apply_event(&UserEvent::Edit {
                        id,
                        text: text.clone(),
                    });
                    self.user_blocks.insert(line.block_id.clone(), (id, text));
                }
                Some(_) => {}
                None => {
                    let id = self.next_user + 1;
                    let added = self.living.apply_event(&UserEvent::Add {
                        id,
                        t: line.ms.map(format_clock),
                        text: text.clone(),
                    });
                    // Before the first topic the line waits for the next read.
                    if added {
                        self.next_user = id;
                        self.user_blocks
                            .insert(line.block_id.clone(), (NoteId::User(id), text));
                    }
                }
            }
        }
        removed
    }

    fn note_of_entry(&self, entry_id: &str) -> Option<NoteId> {
        self.entries
            .iter()
            .find(|(_, e)| e.id == entry_id)
            .map(|(id, _)| *id)
    }

    /// Entries whose notes are dropped: they leave the timeline.
    fn forget_dropped(&mut self) -> Vec<String> {
        let dropped: Vec<NoteId> = self
            .entries
            .keys()
            .copied()
            .filter(|id| self.living.note(*id).is_some_and(|n| n.dropped))
            .collect();
        let mut removed: Vec<String> = dropped
            .iter()
            .filter_map(|id| self.entries.remove(id))
            .map(|e| e.id)
            .collect();
        removed.sort();
        removed
    }

    /// The user turn for `window`.
    pub fn prompt(&self, window: &WindowText) -> String {
        let closed = &self.closed[self.closed.len().saturating_sub(CLOSED_NOTES_IN_PROMPT)..];
        render_user(
            window,
            &self.living,
            self.living.topic.as_deref(),
            self.since.as_deref(),
            closed,
        )
    }

    /// Applies the model's answer for `window` and returns what changes in
    /// the timeline. An answer that does not parse changes nothing.
    pub fn apply_answer(
        &mut self,
        raw: &str,
        window: &Window,
        provider: &str,
        model: &str,
    ) -> (Vec<EntryChange>, Vec<Applied>) {
        let decoded = match decode(raw, self.living.maxid + 1) {
            Ok(decoded) => decoded,
            // `_error` only reaches debug builds' log (perf_debug).
            Err(_error) => {
                // The answer holds meeting content: only its size reaches
                // the release log.
                log::warn!(
                    "Live notes: skipping an unreadable answer ({} chars)",
                    raw.len()
                );
                perf_debug!("Live notes unreadable answer: {_error}");
                Decoded::default()
            }
        };
        if let Some(topic) = decoded.topic {
            // The same title again is no new topic (the validator's rule).
            let same = self
                .living
                .topic
                .as_deref()
                .is_some_and(|current| same_title(current, &topic));
            if !same {
                self.closed.extend(self.living.open_texts());
                self.living.new_topic();
                self.living.topic = Some(topic);
                self.since = Some(format_clock(window.start_ms));
            }
        }
        let before: HashMap<NoteId, NoteState> = self
            .entries
            .keys()
            .filter_map(|id| self.living.note(*id).map(|n| (*id, state(n))))
            .collect();
        let recent_closed: Vec<String> = self
            .closed
            .iter()
            .rev()
            .take(CLOSED_REPEAT_HISTORY)
            .cloned()
            .collect();
        let mut applied = Vec::new();
        for op in decoded.ops {
            // A new note may not repeat what an earlier topic already noted.
            let repeat = match &op {
                Op::Add(note) | Op::Child { note, .. } => recent_closed
                    .iter()
                    .any(|old| super::parse::is_repeat(&note.text, old)),
                _ => false,
            };
            if repeat {
                applied.push(Applied {
                    op,
                    rejected: vec!["repeats_an_earlier_topic".into()],
                    repaired_from: Vec::new(),
                });
                continue;
            }
            applied.push(apply_op(&mut self.living, op));
        }
        let mut changes = Vec::new();
        let created_at = chrono::Utc::now().to_rfc3339();
        for applied in applied.iter().filter(|a| a.rejected.is_empty()) {
            let id = match &applied.op {
                Op::Add(note) | Op::Child { note, .. } => NoteId::Ai(note.id),
                _ => continue,
            };
            // As it stands after the whole answer (a later line may edit it).
            let Some(note) = self.living.note(id).filter(|n| !n.dropped) else {
                continue;
            };
            let clock = |time: &Option<String>| time.as_deref().and_then(clock_ms);
            let start_ms = clock(&note.start)
                .unwrap_or(window.start_ms)
                .min(window.end_ms);
            let end_ms = clock(&note.end)
                .unwrap_or(start_ms)
                .clamp(start_ms, window.end_ms.max(start_ms));
            let parent = match &applied.op {
                Op::Child { parent, .. } => self.parent_ref(*parent),
                _ => None,
            };
            let entry = TimelineEntry {
                id: uuid::Uuid::new_v4().to_string(),
                start_ms,
                end_ms,
                kind: note.kind.as_str().to_string(),
                text: note.text.clone(),
                speaker: note.owner.clone().filter(|_| note.kind == Kind::Action),
                provider: provider.to_string(),
                model: model.to_string(),
                created_at: created_at.clone(),
                topic: self.living.topic.clone(),
                parent,
            };
            self.entries.insert(id, entry.clone());
            changes.push(EntryChange::Add(entry));
        }
        let mut edited: Vec<NoteId> = before
            .iter()
            .filter(|(id, old)| {
                self.living
                    .note(**id)
                    .is_some_and(|n| !n.dropped && state(n) != **old)
            })
            .map(|(id, _)| *id)
            .collect();
        edited.sort_by_key(|id| self.living.order.iter().position(|j| j == id));
        for id in edited {
            let note = self.living.note(id).expect("edited").clone();
            let entry = self.entries.get_mut(&id).expect("an entry");
            entry.text = note.text.clone();
            entry.kind = note.kind.as_str().to_string();
            entry.speaker = note.owner.clone().filter(|_| note.kind == Kind::Action);
            if let Some(end) = note.end.as_deref().and_then(clock_ms) {
                entry.end_ms = end.max(entry.start_ms);
            }
            changes.push(EntryChange::Update(entry.clone()));
        }
        changes.extend(self.forget_dropped().into_iter().map(EntryChange::Remove));
        (changes, applied)
    }

    /// What a sub-bullet's entry names as its parent: the parent note's
    /// entry, or the user's line (its editor block).
    fn parent_ref(&self, parent: NoteId) -> Option<String> {
        match parent {
            NoteId::Ai(_) => self.entries.get(&parent).map(|e| e.id.clone()),
            NoteId::User(_) => self
                .user_blocks
                .iter()
                .find(|(_, (id, _))| *id == parent)
                .map(|(block, _)| block.clone()),
        }
    }
}

/// Whether two topic titles are the same once case and punctuation go.
fn same_title(a: &str, b: &str) -> bool {
    let norm = |t: &str| {
        t.to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == ' ')
            .collect::<String>()
            .trim()
            .to_string()
    };
    norm(a) == norm(b)
}

fn state(note: &Note) -> NoteState {
    (
        note.text.clone(),
        note.kind,
        note.owner.clone(),
        note.end.clone(),
        note.dropped,
    )
}

#[cfg(test)]
mod tests;
