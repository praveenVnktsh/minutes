//! Turning the model's reply into timeline entries. Pure and defensive:
//! small local models wrap JSON in fences or think tags, run out of tokens
//! mid-object, invent kinds and times. Whatever is usable is kept; output with
//! nothing usable is an error the worker logs and skips.

use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

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
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoJson => write!(f, "the model reply held no JSON"),
            Self::UnexpectedShape => write!(f, "the model reply held no timeline entries"),
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
    let value = reply_json_value(&cleaned).or_else(|| reply_json_value(&repair_brackets(&cleaned)));
    let items = match value {
        Some(value) => entry_items(value).ok_or(ParseError::UnexpectedShape)?,
        None => {
            let salvaged = salvage_objects(&cleaned);
            if salvaged.is_empty() {
                return Err(if cleaned.contains('{') || cleaned.contains('[') {
                    ParseError::UnexpectedShape
                } else {
                    ParseError::NoJson
                });
            }
            salvaged
        }
    };

    let window_end_ms = window_end_ms.max(window_start_ms);
    let mut refiner = context.map(Refiner::new);
    let mut seen: Vec<HashSet<String>> = previous_texts.iter().map(|t| word_set(t)).collect();
    let mut entries = Vec::new();
    for item in items {
        if entries.len() >= max_entries {
            break;
        }
        let Some(entry) = entry_from(&item, window_start_ms, window_end_ms) else {
            continue;
        };
        let entry = match refiner.as_mut() {
            Some(refiner) => match refiner.refine(entry) {
                Some(entry) => entry,
                None => continue,
            },
            None => entry,
        };
        let words = word_set(&entry.text);
        if words.is_empty()
            || seen
                .iter()
                .any(|other| overlap(&words, other) >= DUPLICATE_OVERLAP)
        {
            continue;
        }
        if let Some(refiner) = refiner.as_mut() {
            refiner.accept(&entry);
        }
        seen.push(words);
        entries.push(entry);
    }
    Ok(entries)
}

/// Remove `<think>…</think>` blocks. An unclosed `<think>` (the model ran out
/// of tokens, or the template opened it) is dropped up to the first JSON.
fn strip_think(raw: &str) -> String {
    let mut text = raw.to_string();
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
