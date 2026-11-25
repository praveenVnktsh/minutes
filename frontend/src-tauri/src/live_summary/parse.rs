//! Turning the model's reply into timeline entries. Pure and defensive:
//! small local models wrap JSON in fences or think tags, run out of tokens
//! mid-object, invent kinds and times. Whatever is usable is kept; output with
//! nothing usable is an error the worker logs and skips.

use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

use super::facts::Figures;
use super::notes::{NoteContext, Refiner};

/// Longest entry text kept, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// Longest free-form speaker label kept, in characters.
const MAX_SPEAKER_CHARS: usize = 40;
/// Word-set overlap at which two entries count as the same.
const DUPLICATE_OVERLAP: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Point,
    Decision,
    Action,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Point => "point",
            Self::Decision => "decision",
            Self::Action => "action",
        }
    }

    fn from_label(label: &str) -> Self {
        let label = label.to_ascii_lowercase();
        if ["action", "todo", "to-do", "task", "follow"]
            .iter()
            .any(|word| label.contains(word))
        {
            Self::Action
        } else if ["decision", "decided", "agree"]
            .iter()
            .any(|word| label.contains(word))
        {
            Self::Decision
        } else {
            Self::Point
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: EntryKind,
    pub text: String,
    pub speaker: Option<String>,
    /// The section the note belongs to, as the model named it (refined by
    /// `parse_notes`).
    pub topic: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// No JSON object or array anywhere in the reply.
    NoJson,
    /// JSON was found but held no entry list.
    UnexpectedShape,
    /// A compact reply whose note lines could not be read.
    UnreadableLines,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoJson => write!(f, "the model reply held no JSON"),
            Self::UnexpectedShape => write!(f, "the model reply held no timeline entries"),
            Self::UnreadableLines => write!(f, "the model reply's note lines could not be read"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Entries from `raw` for a window spanning `window_start_ms..=window_end_ms`.
/// Times are clamped into the window, entries repeating `previous_texts` (or
/// each other) are dropped, and at most `max_entries` are kept. An explicit
/// empty list is a valid "nothing new" answer.
///
/// This is the reply as the model wrote it; `parse_notes` also applies the
/// notetaker rules (topics, no narration, strict kinds, names).
pub fn parse_entries(
    raw: &str,
    window_start_ms: i64,
    window_end_ms: i64,
    max_entries: usize,
    previous_texts: &[String],
) -> Result<Vec<ParsedEntry>, ParseError> {
    parse_with(
        raw,
        window_start_ms,
        window_end_ms,
        max_entries,
        previous_texts,
        None,
    )
}

/// Like `parse_entries`, with the notetaker rules of `notes::Refiner`:
/// topics resolved against the current one, narration rewritten or dropped,
/// actions and decisions held to their definitions, names spelled as in the
/// glossary. Repeats are judged on the refined text.
pub fn parse_notes(
    raw: &str,
    window_start_ms: i64,
    window_end_ms: i64,
    max_entries: usize,
    context: &NoteContext<'_>,
) -> Result<Vec<ParsedEntry>, ParseError> {
    parse_with(
        raw,
        window_start_ms,
        window_end_ms,
        max_entries,
        context.previous_texts,
        Some(context),
    )
}

fn parse_with(
    raw: &str,
    window_start_ms: i64,
    window_end_ms: i64,
    max_entries: usize,
    previous_texts: &[String],
    context: Option<&NoteContext<'_>>,
) -> Result<Vec<ParsedEntry>, ParseError> {
    let cleaned = strip_fences(&strip_think(raw));
    let items = match compact_items(&cleaned) {
        Some(Compact::Items(items)) => items,
        Some(Compact::Unreadable) => return Err(ParseError::UnreadableLines),
        None => json_items(&cleaned)?,
    };
    refine_items(
        items,
        window_start_ms,
        window_end_ms,
        max_entries,
        previous_texts,
        context,
    )
}

/// A reply in the compact format.
enum Compact {
    /// Its notes (none for an explicit `none` or a bare topic line).
    Items(Vec<Value>),
    /// It has a topic line or `none`, but its other lines are not notes: an
    /// unreadable window to retry, not "nothing new".
    Unreadable,
}

/// The compact answer (`prompt::compact_system_prompt`): a `topic: …` line,
/// then `none` or lines `mm:ss|kind|owner|note`. Only the three leading
/// fields are split off: the note is the rest of the line, pipes included.
/// `None` when the reply is not in that format.
fn compact_items(text: &str) -> Option<Compact> {
    let mut saw_topic = false;
    let mut saw_none = false;
    let mut unreadable = 0;
    let mut items = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        if line.is_empty() {
            continue;
        }
        if topic_line(line).is_some() {
            saw_topic = true;
            continue;
        }
        let bare = line.trim_end_matches('.').to_ascii_lowercase();
        if matches!(bare.as_str(), "none" | "nothing" | "nothing new") {
            saw_none = true;
            continue;
        }
        match compact_note(line) {
            Some(item) => items.push(item),
            None => unreadable += 1,
        }
    }
    if !items.is_empty() {
        return Some(Compact::Items(items));
    }
    if saw_none {
        return Some(Compact::Items(items));
    }
    if text.contains('{') {
        return None;
    }
    if saw_topic {
        return Some(if unreadable > 0 {
            Compact::Unreadable
        } else {
            Compact::Items(items)
        });
    }
    None
}

/// One note line: `mm:ss|kind|note`, with the owner before the task for an
/// action (`mm:ss|action|owner|task`). An owner column on a point or a
/// decision (`mm:ss|point|-|note`) is accepted and dropped. Without a time
/// (`kind|…`) the note is placed by its words ([`align_start`]).
fn compact_note(line: &str) -> Option<Value> {
    let first = line.split('|').next()?.trim();
    let timed = parse_clock_ms(first).is_some();
    if !timed && !is_kind_field(first) {
        return None;
    }
    let mut rest = line;
    let start = if timed {
        let (start, after) = rest.split_once('|')?;
        rest = after;
        start.trim()
    } else {
        ""
    };
    let (kind, after) = rest.split_once('|')?;
    let kind = kind.trim();
    let mut note = after.trim();
    let mut owner = "";
    if kind.eq_ignore_ascii_case("action") {
        if let Some((field, after)) = note.split_once('|') {
            owner = field.trim();
            note = after.trim();
        }
    }
    // Owner columns on a point, and the transcript's speaker label a small
    // model sometimes adds as a column (`04:12|point|-|Others|…`): not the
    // note's first word. An action's owner in the wrong column is its owner.
    while let Some((field, after)) = note.split_once('|') {
        let field = field.trim();
        if !is_owner_field(field) {
            break;
        }
        if matches!(owner, "" | "-") && kind.eq_ignore_ascii_case("action") {
            owner = field;
        }
        note = after.trim();
    }
    if note.is_empty() || is_owner_field(note) {
        return None;
    }
    let owner = match owner {
        "" | "-" | "none" | "null" => Value::Null,
        owner => Value::String(owner.to_string()),
    };
    let mut item = serde_json::json!({
        "kind": kind,
        "speaker": owner,
        "text": note,
    });
    if timed {
        item["start"] = Value::String(start.to_string());
    }
    Some(item)
}

/// A compact line's kind field.
fn is_kind_field(field: &str) -> bool {
    matches!(
        field.to_ascii_lowercase().as_str(),
        "point" | "decision" | "action"
    )
}

/// Where a note written without a time starts: the transcript segment that
/// shares the most content words with it (the earliest on a tie), or `None`
/// when no segment shares any.
pub fn align_start(text: &str, segments: &[super::scheduler::Segment]) -> Option<i64> {
    let note = content_words(&word_set(text));
    if note.is_empty() {
        return None;
    }
    let mut best: Option<(usize, i64)> = None;
    for segment in segments {
        let shared = content_words(&word_set(&segment.text))
            .intersection(&note)
            .count();
        if shared > 0 && best.is_none_or(|(most, _)| shared > most) {
            best = Some((shared, segment.start_ms));
        }
    }
    best.map(|(_, start_ms)| start_ms)
}

/// A field that can only be an owner or a transcript label, never a note.
fn is_owner_field(field: &str) -> bool {
    matches!(
        field.to_ascii_lowercase().as_str(),
        "" | "-" | "you" | "others" | "none" | "null" | "speaker"
    )
}

/// The title after `topic:`, if `line` is a topic line.
fn topic_line(line: &str) -> Option<&str> {
    let (label, rest) = line.split_once(':')?;
    label
        .trim()
        .eq_ignore_ascii_case("topic")
        .then(|| rest.trim().trim_matches('"'))
}

/// The topic line of a compact answer: `same`, or the new topic.
pub fn compact_topic(raw: &str) -> Option<TopicDecision> {
    let cleaned = strip_fences(&strip_think(raw));
    let title = cleaned
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', ' ']))
        .find_map(topic_line)?;
    let bare = title.trim_end_matches('.').trim();
    if bare.is_empty() {
        return None;
    }
    if bare.eq_ignore_ascii_case("same") {
        return Some(TopicDecision::Same);
    }
    Some(TopicDecision::New(bare.to_string()))
}

/// The entry objects of a JSON reply.
fn json_items(cleaned: &str) -> Result<Vec<Value>, ParseError> {
    // The reply as written and with its brackets repaired: the one with more
    // entries wins, so `[{a},{"text":"b"]}` keeps b rather than stopping at
    // the first object that happens to parse.
    let direct = reply_json_value(cleaned);
    let repaired = reply_json_value(&repair_brackets(cleaned));
    let count = |value: &Option<Value>| {
        value
            .clone()
            .and_then(entry_items)
            .map_or(0, |items| items.len())
    };
    let value = if count(&repaired) > count(&direct) {
        repaired
    } else {
        direct.or(repaired)
    };
    match value {
        Some(value) => entry_items(value).ok_or(ParseError::UnexpectedShape),
        None => {
            let salvaged = salvage_objects(cleaned);
            if salvaged.is_empty() {
                return Err(if cleaned.contains('{') || cleaned.contains('[') {
                    ParseError::UnexpectedShape
                } else {
                    ParseError::NoJson
                });
            }
            Ok(salvaged)
        }
    }
}

fn refine_items(
    items: Vec<Value>,
    window_start_ms: i64,
    window_end_ms: i64,
    max_entries: usize,
    previous_texts: &[String],
    context: Option<&NoteContext<'_>>,
) -> Result<Vec<ParsedEntry>, ParseError> {
    let window_end_ms = window_end_ms.max(window_start_ms);
    let mut refiner = context.map(Refiner::new);
    let mut seen: Vec<HashSet<String>> = previous_texts.iter().map(|t| word_set(t)).collect();
    let mut seen_texts: Vec<String> = previous_texts.to_vec();
    let mut entries = Vec::new();
    for item in items {
        if entries.len() >= max_entries {
            break;
        }
        let Some(mut entry) = entry_from(&item, window_start_ms, window_end_ms) else {
            continue;
        };
        if item.get("start").is_none() {
            if let Some(start_ms) =
                context.and_then(|context| align_start(&entry.text, context.segments))
            {
                entry.start_ms = start_ms.clamp(window_start_ms, window_end_ms);
            }
        }
        let entry = match refiner.as_mut() {
            Some(refiner) => match refiner.refine(entry) {
                Some(entry) => entry,
                None => continue,
            },
            None => entry,
        };
        let words = word_set(&entry.text);
        let repeated = if context.is_some() {
            // The notetaker: rewordings are repeats, changed facts are not.
            seen_texts.iter().any(|other| is_repeat(&entry.text, other))
        } else {
            // The A/B baseline stays as it was.
            seen.iter()
                .any(|other| overlap(&words, other) >= DUPLICATE_OVERLAP)
        };
        if words.is_empty() || repeated {
            continue;
        }
        if let Some(refiner) = refiner.as_mut() {
            refiner.accept(&entry);
        }
        seen.push(words);
        seen_texts.push(entry.text.clone());
        entries.push(entry);
    }
    Ok(entries)
}

/// The topic step's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum TopicDecision {
    Same,
    New(String),
}

/// `{"same":true}` or `{"same":false,"topic":"…"}`; `None` for anything
/// else (the notes call then names the topic itself).
pub fn parse_topic_decision(raw: &str) -> Option<TopicDecision> {
    let cleaned = strip_fences(&strip_think(raw));
    let value = reply_json_value(&cleaned).or_else(|| {
        let start = cleaned.find('{')?;
        let end = balanced_end(cleaned.as_bytes(), start)?;
        serde_json::from_str(&cleaned[start..=end]).ok()
    })?;
    let same = match &value["same"] {
        Value::Bool(same) => *same,
        Value::String(text) => matches!(text.trim().to_lowercase().as_str(), "true" | "yes"),
        _ => return None,
    };
    if same {
        return Some(TopicDecision::Same);
    }
    let topic = value["topic"].as_str()?.trim();
    (!topic.is_empty()).then(|| TopicDecision::New(topic.to_string()))
}

/// Remove `<think>…</think>` blocks. An unclosed `<think>` (the model ran out
/// of tokens, or the template opened it) is dropped up to the first JSON.
fn strip_think(raw: &str) -> String {
    let mut text = raw.to_string();
    // Gemma 4's thought channel.
    while let Some(open) = text.find("<|channel>") {
        match text[open..].find("<channel|>") {
            Some(close) => text.replace_range(open..open + close + "<channel|>".len(), ""),
            None => text.truncate(open),
        }
    }
    while let Some(open) = text.find("<think>") {
        match text[open..].find("</think>") {
            Some(close) => text.replace_range(open..open + close + "</think>".len(), ""),
            None => {
                let rest = &text[open + "<think>".len()..];
                let json_start = rest.find(['{', '[']).map(|i| open + "<think>".len() + i);
                match json_start {
                    Some(start) => text.replace_range(open..start, ""),
                    None => text.truncate(open),
                }
            }
        }
    }
    text.replace("</think>", "")
}

fn strip_fences(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The JSON the reply answers with: the first complete object carrying an
/// `entries` key, else the first complete JSON object or array at all. Models
/// sometimes echo a fragment of the prompt (a time like `[01:05]`, an example
/// object) before the real answer, so the first value alone is not enough.
fn reply_json_value(text: &str) -> Option<Value> {
    let values = json_values(text);
    let entries_at = values.iter().position(|value| {
        value
            .as_object()
            .is_some_and(|map| map.contains_key("entries"))
    });
    match entries_at {
        Some(index) => values.into_iter().nth(index),
        None => values.into_iter().next(),
    }
}

/// Every complete top-level JSON object or array in `text`, in order.
fn json_values(text: &str) -> Vec<Value> {
    let mut values = Vec::new();
    let mut offset = 0;
    while let Some(index) = text[offset..].find(['{', '[']) {
        let start = offset + index;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value)) => {
                values.push(value);
                offset = start + stream.byte_offset().max(1);
            }
            _ => offset = start + 1,
        }
    }
    values
}

/// The entry list in a parsed reply: `{"entries": [...]}`, any other single
/// array field, a bare array, or one bare entry object.
fn entry_items(value: Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(items) => Some(items),
        Value::Object(mut map) => {
            if let Some(Value::Array(items)) = map.remove("entries") {
                return Some(items);
            }
            if map.contains_key("text") {
                return Some(vec![Value::Object(map)]);
            }
            map.into_iter().find_map(|(_, value)| match value {
                Value::Array(items) => Some(items),
                _ => None,
            })
        }
        _ => None,
    }
}

/// Complete `{…}` objects with a `text` field, from a reply whose JSON was
/// cut off (the output cap is small).
fn salvage_objects(text: &str) -> Vec<Value> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        match balanced_end(bytes, index) {
            Some(end) => match serde_json::from_str::<Value>(&text[index..=end]) {
                Ok(value) if value.get("text").is_some() => {
                    found.push(value);
                    index = end + 1;
                }
                _ => index += 1,
            },
            None => index += 1,
        }
    }
    found
}

/// `text` with mismatched closing brackets fixed, string-aware: small models
/// write `{"entries":[{"text":"…"]}`, dropping the object's `}`. A `]` that
/// meets an open `{` closes it first; a `}` that meets an open `[` closes
/// the array first. Brackets still open at the end are closed.
fn repair_brackets(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    let mut open: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => open.push(c),
            '}' | ']' => {
                let wanted = if c == '}' { '{' } else { '[' };
                while let Some(&top) = open.last() {
                    if top == wanted {
                        break;
                    }
                    out.push(if top == '{' { '}' } else { ']' });
                    open.pop();
                }
                open.pop();
            }
            _ => {}
        }
        out.push(c);
    }
    if !in_string {
        while let Some(top) = open.pop() {
            out.push(if top == '{' { '}' } else { ']' });
        }
    }
    out
}

/// Index of the `}` closing the `{` at `start`, string-aware.
fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn entry_from(item: &Value, window_start_ms: i64, window_end_ms: i64) -> Option<ParsedEntry> {
    let text = ["text", "summary", "title"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .map(clean_text)?;
    if text.is_empty() {
        return None;
    }
    let kind = ["kind", "type"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .map(EntryKind::from_label)
        .unwrap_or(EntryKind::Point);

    let clamp = |ms: i64| ms.clamp(window_start_ms, window_end_ms);
    let start_ms = time_field(item, &["start", "start_time"], "start_ms")
        .map(clamp)
        .unwrap_or(window_start_ms);
    let end_ms = time_field(item, &["end", "end_time"], "end_ms")
        .map(clamp)
        .unwrap_or(window_end_ms)
        .max(start_ms);

    Some(ParsedEntry {
        start_ms,
        end_ms,
        kind,
        text,
        speaker: item.get("speaker").and_then(speaker_label),
        topic: ["topic", "section", "chapter"]
            .iter()
            .find_map(|key| item.get(*key).and_then(Value::as_str))
            .map(|topic| topic.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|topic| !topic.is_empty()),
    })
}

/// A time in ms from `keys` (clock strings or seconds) or `ms_key` (milliseconds).
fn time_field(item: &Value, keys: &[&str], ms_key: &str) -> Option<i64> {
    if let Some(ms) = item.get(ms_key).and_then(Value::as_f64) {
        return finite_ms(ms);
    }
    keys.iter().find_map(|key| match item.get(*key)? {
        Value::Number(number) => finite_ms(number.as_f64()? * 1000.0),
        Value::String(text) => parse_clock_ms(text),
        _ => None,
    })
}

fn finite_ms(ms: f64) -> Option<i64> {
    (ms.is_finite() && ms >= 0.0).then_some(ms.round() as i64)
}

/// `mm:ss`, `h:mm:ss`, `ss`, `12.5`, `12s`, with optional brackets.
pub fn parse_clock_ms(text: &str) -> Option<i64> {
    let text = text
        .trim()
        .trim_matches(|c| c == '[' || c == ']')
        .trim_end_matches('s')
        .trim();
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0;
    for part in &parts {
        let value: f64 = part.trim().parse().ok()?;
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        seconds = seconds * 60.0 + value;
    }
    finite_ms(seconds * 1000.0)
}

fn speaker_label(value: &Value) -> Option<String> {
    let label = value.as_str()?.trim();
    match label.to_ascii_lowercase().as_str() {
        "" | "null" | "none" | "unknown" | "n/a" => None,
        "you" | "me" | "mic" | "microphone" => Some("You".to_string()),
        "others" | "other" | "system" => Some("Others".to_string()),
        _ => Some(label.chars().take(MAX_SPEAKER_CHARS).collect()),
    }
}

fn clean_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_start_matches(|c: char| c == '-' || c == '*' || c == '•' || c.is_whitespace())
        .trim();
    if trimmed.chars().count() <= MAX_TEXT_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_TEXT_CHARS).collect();
    match cut.rfind(' ') {
        Some(space) if space > MAX_TEXT_CHARS / 2 => format!("{}…", &cut[..space]),
        _ => format!("{cut}…"),
    }
}

fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Words that carry no content for the repeat check.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "are", "was", "were", "has", "have",
    "had", "not", "but", "its", "into", "than", "then", "over", "about", "also", "very", "more",
    "most", "now", "their", "they", "them", "will", "can", "all", "any", "out", "who", "what",
    "our", "your", "you", "his", "her", "she", "been", "being", "is", "of", "to", "in", "on", "at",
    "by", "as", "or", "an", "a", "it", "be", "vs", "via", "per",
];
/// A repeat shares at least this many content words with an earlier note…
const REPEAT_MIN_SHARED: usize = 4;
/// …and at least this share of the shorter note's content words…
const REPEAT_SHARE: f64 = 0.4;
/// …and of both notes' content words together.
const REPEAT_JACCARD: f64 = 0.3;

/// Content words, crudely stemmed ("bets" → "bet"), numbers kept.
fn content_words(words: &HashSet<String>) -> HashSet<String> {
    words
        .iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .filter(|w| w.len() >= 3 || w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| match w.strip_suffix('s') {
            Some(stem) if stem.len() >= 3 && !w.ends_with("ss") => stem.to_string(),
            _ => w.clone(),
        })
        .collect()
}

/// Whether two notes say the same thing in other words: they share
/// `REPEAT_MIN_SHARED` content words, `REPEAT_SHARE` of the shorter one's.
fn shares_content(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    let (a, b) = (content_words(a), content_words(b));
    let shared = a.intersection(&b).count();
    let shorter = a.len().min(b.len()).max(1);
    let union = a.union(&b).count().max(1);
    shared >= REPEAT_MIN_SHARED
        && shared as f64 / shorter as f64 >= REPEAT_SHARE
        && shared as f64 / union as f64 >= REPEAT_JACCARD
}

/// Whether `text` repeats `other` (the notetaker's repeat check): the same
/// words, or a rewording that shares most content words. Notes whose figures
/// differ ("May 10" → "May 24") or where only one is negated ("will ship" →
/// "will not ship") say something new, however alike they read.
pub fn is_repeat(text: &str, other: &str) -> bool {
    let (a, b) = (word_set(text), word_set(other));
    let alike = overlap(&a, &b) >= DUPLICATE_OVERLAP || shares_content(&a, &b);
    alike && !figures_differ(text, other) && negated(text) == negated(other)
}

/// Words and endings that negate a note.
const NEGATIONS: &[&str] = &[
    "not",
    "no",
    "never",
    "none",
    "nobody",
    "nothing",
    "neither",
    "nor",
    "without",
    "cannot",
    "won't",
    "can't",
    "don't",
    "doesn't",
    "didn't",
    "isn't",
    "aren't",
    "wasn't",
    "weren't",
    "shouldn't",
    "wouldn't",
    "couldn't",
    "hasn't",
    "haven't",
    "hadn't",
    "cancelled",
    "canceled",
    "dropped",
    "scrapped",
];

/// Whether a note is negated ("not", "won't", "no longer", "never"…).
fn negated(text: &str) -> bool {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
                .replace('’', "'")
                .to_lowercase()
        })
        .any(|w| NEGATIONS.contains(&w.as_str()) || w.ends_with("n't"))
}

/// Whether two notes state different figures: one has a value the other
/// does not.
fn figures_differ(a: &str, b: &str) -> bool {
    let (a, b) = (Figures::of(a), Figures::of(b));
    let missing = |from: &Figures, other: &Figures| {
        from.values().iter().any(|v| {
            !other
                .values()
                .iter()
                .any(|w| (v - w).abs() < 1e-6 * v.abs().max(1.0))
        })
    };
    missing(&a, &b) || missing(&b, &a)
}

fn overlap(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count() as f64;
    shared / a.union(b).count() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 60_000;
    const END: i64 = 120_000;

    fn parse(raw: &str) -> Result<Vec<ParsedEntry>, ParseError> {
        parse_entries(raw, START, END, 3, &[])
    }

    #[test]
    fn compact_lines_are_parsed() {
        let raw = "topic: Launch plan\n\
                   01:10|point|-|Offline sync bug is the last blocker\n\
                   01:40|action|You|send support the launch announcement\n";
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].start_ms, 70_000);
        assert_eq!(entries[0].kind, EntryKind::Point);
        assert_eq!(entries[0].speaker, None);
        assert_eq!(entries[0].topic, None);
        assert_eq!(entries[0].text, "Offline sync bug is the last blocker");
        assert_eq!(entries[1].kind, EntryKind::Action);
        assert_eq!(entries[1].speaker.as_deref(), Some("You"));
        // With a bullet in front.
        let entries = parse("- 01:05|decision|-|Budget approved by finance").unwrap();
        assert_eq!(entries[0].kind, EntryKind::Decision);
        assert_eq!(entries[0].text, "Budget approved by finance");
    }

    #[test]
    fn a_pipe_inside_a_compact_note_stays_in_its_text() {
        let raw = "topic: same\n01:10|point|-|Plan A | Plan B both stay on the table\n";
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "Plan A | Plan B both stay on the table");
        assert_eq!(entries[0].topic, None);
        assert_eq!(entries[0].speaker, None);
        let entries = parse("01:20|action|You|send the deck|pricing to finance").unwrap();
        assert_eq!(entries[0].text, "send the deck|pricing to finance");
        assert_eq!(entries[0].speaker.as_deref(), Some("You"));
    }

    #[test]
    fn a_speaker_label_written_as_an_extra_field_is_not_the_note() {
        let raw = "topic: same\n\
                   01:10|point|-|Others|Hiring paused until spring\n\
                   01:20|action|-|You|send the deck to finance\n\
                   01:30|action|Others|Others|introduce You to the founders\n";
        let entries = parse(raw).unwrap();
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Hiring paused until spring",
                "send the deck to finance",
                "introduce You to the founders"
            ]
        );
        assert_eq!(entries[0].speaker, None);
        // An action's owner in the wrong column is still its owner.
        assert_eq!(entries[1].speaker.as_deref(), Some("You"));
        assert_eq!(entries[2].speaker.as_deref(), Some("Others"));
        let entries = parse("01:40|point|-|-|Budget doubles to $2M").unwrap();
        assert_eq!(entries[0].text, "Budget doubles to $2M");
        // A pipe later in the note is kept.
        let entries = parse("01:10|point|-|Plan A|Plan B stay open").unwrap();
        assert_eq!(entries[0].text, "Plan A|Plan B stay open");
    }

    #[test]
    fn a_placeholder_topic_is_no_topic() {
        for title in ["none yet", "None Yet", "unknown", "TBD"] {
            assert_eq!(
                crate::live_summary::notes::normalize_topic(title),
                None,
                "{title}"
            );
        }
    }

    #[test]
    fn a_topic_line_with_only_malformed_notes_is_unreadable() {
        for raw in [
            "topic: Budget\nBudget approved by finance",
            "topic: same\n1:1O|point|-|Hiring paused until spring",
            "topic: same\n01:10|point|-",
            "topic: same\n01:10 point Hiring paused until spring",
        ] {
            assert_eq!(parse(raw), Err(ParseError::UnreadableLines), "{raw}");
        }
        // One readable line is enough; the malformed one is skipped.
        let entries =
            parse("topic: same\ngarbage\n01:10|point|-|Hiring paused until spring").unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn compact_none_is_an_empty_answer() {
        for raw in [
            "topic: same\nnone",
            "topic: Budget\nNone.",
            "none",
            " none\n",
            "topic: same",
        ] {
            assert_eq!(parse(raw).unwrap(), Vec::new(), "{raw}");
        }
        assert!(parse("I have nothing to add").is_err());
    }

    #[test]
    fn fragments_are_dropped_but_short_actions_and_decisions_stay() {
        let raw = "topic: same\n\
                   02:06|action|Others|do that\n\
                   02:07|point|-|Agreed\n\
                   02:08|point|-|Dates confirmed\n\
                   02:09|action|You|Email Sarah\n\
                   02:10|action|-|Book venue\n\
                   02:11|decision|-|Budget approved\n\
                   02:12|point|-|Launch moves to May";
        let context = NoteContext {
            drop_fragments: true,
            ..Default::default()
        };
        let kept = parse_notes(raw, START, END, 8, &context).unwrap();
        let texts: Vec<&str> = kept.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "email Sarah",
                "Book venue",
                "Budget approved",
                "Launch moves to May"
            ]
        );
        // Without the flag (the JSON prompt, every other provider) nothing is
        // dropped for being short.
        let kept = parse_notes(raw, START, END, 8, &NoteContext::default()).unwrap();
        assert_eq!(kept.len(), 7);
    }

    #[test]
    fn the_compact_topic_line_is_read() {
        assert_eq!(
            compact_topic("topic: same\nnone"),
            Some(TopicDecision::Same)
        );
        assert_eq!(compact_topic("Topic: Same."), Some(TopicDecision::Same));
        assert_eq!(
            compact_topic("topic: Acme data platform\n01:00|point|-|x"),
            Some(TopicDecision::New("Acme data platform".into()))
        );
        assert_eq!(compact_topic("01:00|point|-|x"), None);
        assert_eq!(compact_topic("{\"entries\":[]}"), None);
    }

    #[test]
    fn a_gemma4_thought_channel_is_dropped() {
        let raw = "<|channel>thought\nhmm<channel|>topic: Budget\n01:10|point|-|Budget approved by finance";
        assert_eq!(parse(raw).unwrap()[0].text, "Budget approved by finance");
    }

    #[test]
    fn plain_json_is_parsed() {
        let entries = parse(
            r#"{"entries":[{"start":"01:05","end":"01:30","kind":"decision","text":"Ship on Friday","speaker":"Others"}]}"#,
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![ParsedEntry {
                start_ms: 65_000,
                end_ms: 90_000,
                kind: EntryKind::Decision,
                text: "Ship on Friday".into(),
                speaker: Some("Others".into()),
                topic: None,
            }]
        );
    }

    #[test]
    fn the_object_with_entries_wins_over_earlier_json() {
        let raw = r#"Looking at [01:05] and {"note":"draft"} first. {"entries":[{"kind":"decision","text":"Ship on Friday"}]}"#;
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "Ship on Friday");
        // Without an entries object, the first value still counts.
        let bare = parse(r#"[{"kind":"point","text":"Budget is fine"}]"#).unwrap();
        assert_eq!(bare.len(), 1);
    }

    #[test]
    fn fences_think_tags_and_chatter_are_tolerated() {
        let raw = "<think>They agreed on Friday.</think>\nSure! Here it is:\n```json\n{\"entries\":[{\"kind\":\"action\",\"text\":\"Priya writes release notes\",\"speaker\":\"you\"}]}\n```\nHope that helps.";
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Action);
        assert_eq!(entries[0].speaker.as_deref(), Some("You"));
        // No times given: the entry spans the window.
        assert_eq!((entries[0].start_ms, entries[0].end_ms), (START, END));
    }

    #[test]
    fn an_unclosed_think_block_before_json_is_dropped() {
        let raw = "<think>hmm {not json\n{\"entries\":[{\"text\":\"Budget review\"}]}";
        assert_eq!(parse(raw).unwrap()[0].text, "Budget review");
    }

    #[test]
    fn a_bare_array_and_a_bare_object_are_accepted() {
        assert_eq!(parse(r#"[{"text":"A"},{"text":"B"}]"#).unwrap().len(), 2);
        assert_eq!(
            parse(r#"{"text":"Only one","type":"todo"}"#).unwrap()[0].kind,
            EntryKind::Action
        );
        assert_eq!(
            parse(r#"{"timeline":[{"text":"Other key"}]}"#)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_empty_list_means_nothing_new() {
        assert_eq!(parse(r#"{"entries":[]}"#).unwrap(), vec![]);
    }

    #[test]
    fn truncated_json_keeps_the_complete_entries() {
        let raw = r#"{"entries":[{"start":"01:01","text":"Hiring plan {draft} reviewed","kind":"point"},{"start":"01:40","text":"Two offers ou"#;
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "Hiring plan {draft} reviewed");
        assert_eq!(entries[0].start_ms, 61_000);
    }

    #[test]
    fn an_object_missing_its_closing_brace_is_repaired() {
        let raw = r#"{"entries":[{"start":"01:05","kind":"point","text":"Budget is fine","topic":"Budget"]}"#;
        let entries = parse(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "Budget is fine");
        assert_eq!(entries[0].topic.as_deref(), Some("Budget"));
        // Brackets inside strings are left alone.
        assert_eq!(
            repair_brackets(r#"{"a":[{"t":"x ] y"]}"#),
            r#"{"a":[{"t":"x ] y"}]}"#
        );
    }

    #[test]
    fn garbage_is_an_error() {
        assert_eq!(parse("I could not find anything."), Err(ParseError::NoJson));
        assert_eq!(parse(""), Err(ParseError::NoJson));
        assert_eq!(parse("<think>still thinking"), Err(ParseError::NoJson));
        assert_eq!(parse(r#"{"note": 3}"#), Err(ParseError::UnexpectedShape));
        assert_eq!(
            parse(r#"{"entries":[{"text""#),
            Err(ParseError::UnexpectedShape)
        );
    }

    #[test]
    fn kinds_are_normalized() {
        let entries = parse(
            r#"[{"text":"a one","kind":"Action Item"},{"text":"b two","kind":"agreed"},{"text":"c three","kind":"chapter"}]"#,
        )
        .unwrap();
        let kinds: Vec<_> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [EntryKind::Action, EntryKind::Decision, EntryKind::Point]
        );
    }

    #[test]
    fn times_accept_several_formats_and_are_clamped() {
        assert_eq!(parse_clock_ms("01:05"), Some(65_000));
        assert_eq!(parse_clock_ms("[1:00:05]"), Some(3_605_000));
        assert_eq!(parse_clock_ms("75.5s"), Some(75_500));
        assert_eq!(parse_clock_ms("soon"), None);
        assert_eq!(parse_clock_ms("-3"), None);

        let entries = parse(
            r#"[{"start":5,"end":"99:00","text":"clamped"},{"start_ms":70000,"end_ms":65000,"text":"reversed"}]"#,
        )
        .unwrap();
        assert_eq!((entries[0].start_ms, entries[0].end_ms), (START, END));
        assert_eq!((entries[1].start_ms, entries[1].end_ms), (70_000, 70_000));
    }

    #[test]
    fn repeats_of_earlier_entries_and_of_each_other_are_dropped() {
        let previous = vec!["Team agreed to ship on Friday".to_string()];
        let entries = parse_entries(
            r#"[{"text":"team agreed to ship on friday."},{"text":"New hire starts Monday"},{"text":"New hire starts Monday!"}]"#,
            START,
            END,
            3,
            &previous,
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "New hire starts Monday");
    }

    #[test]
    fn topic_decisions_are_read_leniently() {
        assert_eq!(
            parse_topic_decision(r#"{"same":true}"#),
            Some(TopicDecision::Same)
        );
        assert_eq!(
            parse_topic_decision(
                "<think>hm</think>```json\n{\"same\":false,\"topic\":\"Globex pitch\"}\n```"
            ),
            Some(TopicDecision::New("Globex pitch".into()))
        );
        assert_eq!(
            parse_topic_decision(r#"Sure: {"same":"yes"}"#),
            Some(TopicDecision::Same)
        );
        // A new topic without a title, or no answer at all, is no decision.
        assert_eq!(parse_topic_decision(r#"{"same":false}"#), None);
        assert_eq!(parse_topic_decision(r#"{"entries":[]}"#), None);
        assert_eq!(parse_topic_decision("same"), None);
    }

    #[test]
    fn changed_figures_and_negations_are_not_repeats() {
        let cases = [
            (
                "Launch set for May 10 after the design review",
                "Launch set for May 24 after the design review",
            ),
            ("Launch set for May 10", "Launch slips to May 24"),
            (
                "We will ship the offline sync feature in the beta",
                "We will not ship the offline sync feature in the beta",
            ),
            (
                "Acme will renew the enterprise contract this quarter",
                "Acme won't renew the enterprise contract this quarter",
            ),
            (
                "Pricing tiers still include the free plan for students",
                "Pricing tiers no longer include the free plan for students",
            ),
            (
                "Team of 40 engineers across hardware and software",
                "Team of 100 engineers across hardware and software",
            ),
        ];
        for (earlier, later) in cases {
            assert!(!is_repeat(later, earlier), "{earlier} / {later}");
        }
        // Through the parser: the changed decision is kept.
        let raw = r#"{"entries":[{"kind":"decision","text":"Launch set for May 24 after the design review"}]}"#;
        let previous = vec!["Launch set for May 10 after the design review".to_string()];
        let context = NoteContext {
            previous_texts: &previous,
            ..Default::default()
        };
        let kept = parse_notes(raw, START, END, 2, &context).unwrap();
        assert_eq!(kept.len(), 1);
        // The same words again are still a repeat.
        let previous = vec!["Launch set for May 24 after the design review".to_string()];
        let context = NoteContext {
            previous_texts: &previous,
            ..Default::default()
        };
        assert!(parse_notes(raw, START, END, 2, &context)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn repaired_replies_keep_every_entry() {
        for raw in [
            r#"[{"kind":"point","text":"Budget approved by finance"},{"kind":"point","text":"Hiring paused until spring"]}"#,
            r#"{"entries":[{"kind":"point","text":"Budget approved by finance"},{"kind":"point","text":"Hiring paused until spring"]}"#,
            r#"{"entries":[{"kind":"point","text":"Budget approved by finance"},{"kind":"point","text":"Hiring paused until spring"}"#,
        ] {
            let entries = parse(raw).unwrap();
            let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
            assert_eq!(
                texts,
                ["Budget approved by finance", "Hiring paused until spring"],
                "{raw}"
            );
        }
    }

    #[test]
    fn rewordings_of_a_recent_note_are_repeats() {
        assert!(is_repeat(
            "Moonshot bets risk taking away from core business revenue and cash generation",
            "Moonshot bets risk core revenue; startup vs scale priorities differ",
        ));
        // Same subject, different facts: not repeats.
        for (a, b) in [
            (
                "Team grew from 30 to 40 people and is now approaching 100",
                "Team grows by ten people every week",
            ),
            (
                "Acme raised a Series B from Sequoia with 70 employees",
                "Sequoia partners want founders in their network to meet Acme",
            ),
            (
                "Founders found through networking and vouching rather than resumes",
                "Founders succeed by hard work, not keywords or resumes",
            ),
            (
                "Intersection of software and hardware is a key investment thesis for Acme",
                "Acme investments shifted from SaaS to tangible products like chips, robotics and software for hardware firms",
            ),
        ] {
            assert!(!is_repeat(a, b), "{a} / {b}");
        }
    }

    #[test]
    fn the_entry_count_is_capped() {
        let entries = parse_entries(
            r#"[{"text":"one"},{"text":"two"},{"text":"three"}]"#,
            START,
            END,
            2,
            &[],
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn text_is_cleaned_and_capped_and_empty_entries_are_skipped() {
        let long = "word ".repeat(80);
        let raw = format!(r#"[{{"text":"  - Ship   it  "}},{{"text":"   "}},{{"text":"{long}"}}]"#);
        let entries = parse(&raw).unwrap();
        assert_eq!(entries[0].text, "Ship it");
        assert_eq!(entries.len(), 2);
        assert!(entries[1].text.chars().count() <= MAX_TEXT_CHARS + 1);
        assert!(entries[1].text.ends_with('…'));
    }

    #[test]
    fn speakers_are_normalized() {
        let entries = parse(
            r#"[{"text":"a","speaker":null},{"text":"b","speaker":"null"},{"text":"c","speaker":"Priya"},{"text":"d","speaker":"system"}]"#,
        )
        .unwrap();
        let speakers: Vec<_> = entries.iter().map(|e| e.speaker.as_deref()).collect();
        assert_eq!(speakers, [None, None, Some("Priya")]);
    }
}
