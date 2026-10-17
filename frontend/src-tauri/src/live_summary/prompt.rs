//! The prompts for one live notes window. Pure.
//!
//! The model is a notetaker: it writes headline-style notes of what was said
//! (never who said it), grouped under short topics. Each request carries a
//! little context so names and topics stay consistent across the meeting:
//! the current topic, the last few notes, a glossary of names seen so far,
//! and the tail of the previous window, marked as already covered.

use super::scheduler::{word_count, Segment, Window};

/// How many earlier notes the prompt shows, so the model does not repeat
/// them and keeps their names and topics.
pub const PREVIOUS_ENTRIES_IN_PROMPT: usize = 8;
/// How much of the previous window's transcript is shown as context.
pub const CARRYOVER_MS: i64 = 30_000;
/// A topic older than this is offered as a choice, not as the default.
pub const LONG_TOPIC_MS: i64 = 5 * 60_000;
/// The carried-over context never exceeds this many words.
pub const CARRYOVER_MAX_WORDS: usize = 90;

/// An earlier entry, as the prompt shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviousEntry {
    pub start_ms: i64,
    pub kind: String,
    pub text: String,
    pub topic: Option<String>,
}

/// What the prompt knows besides the window itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct PromptContext<'a> {
    /// The topic of the latest note, which new notes reuse while the subject
    /// stays the same.
    pub current_topic: Option<&'a str>,
    /// Earlier notes, oldest first; only the last few are shown.
    pub previous: &'a [PreviousEntry],
    /// Names and terms seen so far, in the spelling to keep.
    pub glossary: &'a [String],
    /// The tail of the previous window, shown as context only.
    pub carryover: &'a [Segment],
    /// The window's topic, when the topic step decided it.
    pub decided_topic: Option<&'a str>,
}

/// `mm:ss`, or `h:mm:ss` past the hour.
pub fn format_clock(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

pub fn system_prompt() -> &'static str {
    "You are a meeting notetaker. While a meeting is recorded you get the newest part of its \
transcript and write short notes about what was said, grouped under topics.\n\
\n\
How to write a note:\n\
- State WHAT was said, never who said it. Do not start with \"Others\", \"Speaker\", \"They\", \
\"Someone\" or \"You\", and do not use verbs like discusses, mentions, explains, notes, describes.\n\
- Headline style: 6 to 12 words, never more than 14. Facts, names and numbers first. Tidy \
numbers (\"~40 people\", \"$2M\", \"3 weeks\"). One idea per note.\n\
- Numbers only from the new transcript's own lines about that subject: never move a figure \
from one company, team or person to another, and never invent one.\n\
- Few, dense notes. Usually one note per request; a second only for a separate, substantive \
fact. Skip greetings, small talk, filler and anything already noted.\n\
- If nothing new and substantive was said, return {\"entries\":[]}. That is a normal answer.\n\
\n\
Kinds:\n\
- \"action\": a commitment or follow-up someone takes on. It counts when a person says they \
will do something (\"I'll send the deck\"), when \"let's\" or \"we should\" is agreed to, when \
someone offers to do something (\"I can introduce you to the founders\"), or when You accept \
a request. \"speaker\" is the owner: \"You\" for the person recording, a person's name if it \
was said, or \"Others\" when the other side commits. \"text\" is the task alone, starting with \
a verb. Do not miss these.\n\
- \"decision\": the participants explicitly agree on or decide something.\n\
- \"point\": everything else: facts, plans being described, opinions, suggestions nobody took \
on, questions. For points and decisions \"speaker\" is null.\n\
\n\
Topics:\n\
- \"topic\" names what is being discussed NOW, in at most 5 words: the company, product, \
project or person the new transcript is about (\"Acme partnership\", \"Launch plan\", \
\"Priya's background\"). It is a chapter title, not a summary of the note.\n\
- Copy the current topic only while the new transcript is still about that same subject. \
When the subject changes, write a new topic, even in one long conversation. Example: a call \
that moves from Priya's studies and past jobs, to the data platform she builds now, to a \
startup someone pitches her is three topics: \"Priya's background\", \"Acme data platform\", \
\"Globex pitch\". Expect a new topic every 4 to 8 minutes.\n\
\n\
Spell names of people, companies and products as in the glossary, when one is given.\n\
\n\
Transcript labels: \"You\" is the person recording (their microphone); \"Others\" is everyone \
else on the call, possibly several people. The labels can be wrong. Never mention them in a note.\n\
\n\
Examples:\n\
Good: {\"kind\":\"point\",\"text\":\"Offline sync bug is the last launch blocker; fix needs ~3 days\"}\n\
Bad: {\"kind\":\"point\",\"text\":\"Others discuss the sync bug\"} (narrates who spoke, no facts)\n\
Good: {\"kind\":\"point\",\"text\":\"Finance background; previously led pricing at a bank\"}\n\
Bad: {\"kind\":\"point\",\"text\":\"Speaker has a finance background and talks about previous roles at a bank and why they left\"} (about the speaker, too long)\n\
Good: {\"kind\":\"decision\",\"text\":\"Launch moves from the 3rd to the 10th\"}\n\
Good: {\"kind\":\"action\",\"speaker\":\"You\",\"text\":\"send support the launch announcement today\"}\n\
Good: {\"kind\":\"action\",\"speaker\":\"Others\",\"text\":\"introduce You to the Globex founders\"} (\"I can mention our chat to their founders.\" \"Great, thanks.\")\n\
Good: {\"kind\":\"action\",\"speaker\":\"You\",\"text\":\"connect with Acme's robotics lead on LinkedIn\"} (\"Find her on LinkedIn.\" \"Sure, I'll reach out.\")\n\
Bad: {\"kind\":\"action\",\"text\":\"Speaker explains that pricing differs by region\"} (nobody took on a task: a point, and narrated)\n\
\n\
Reply with JSON only, no other text:\n\
{\"entries\":[{\"start\":\"mm:ss\",\"end\":\"mm:ss\",\"topic\":\"...\",\"kind\":\"point\",\"text\":\"...\",\"speaker\":null}]}"
}

/// The user turn: the current topic, the glossary, earlier notes not to
/// repeat, the previous window's tail as context, the window's transcript,
/// and how many notes it may return.
pub fn user_prompt(window: &Window, context: &PromptContext<'_>) -> String {
    let mut prompt = String::new();

    if let Some(decided) = context.decided_topic.filter(|t| !t.trim().is_empty()) {
        prompt.push_str(&format!(
            "Topic of the new transcript: {} (already decided: use it as \"topic\")\n",
            decided.trim()
        ));
    } else if let Some(topic) = context.current_topic.filter(|t| !t.trim().is_empty()) {
        let topic = topic.trim();
        // When the current topic's run of notes started, so the model can
        // tell a chapter that has gone on for long.
        let since = topic_since(context.previous, topic);
        match since {
            // A small model copies the current topic forever. Once it has
            // run long, it is offered as a choice rather than the default,
            // and the parser still reuses it for a near-duplicate title.
            Some(since) if window.start_ms - since > LONG_TOPIC_MS => {
                prompt.push_str(&format!(
                    "Topic so far: {topic} (since {}). Name the topic of the new transcript afresh; repeat this title only if it is still the best fit.\n",
                    format_clock(since)
                ));
            }
            Some(since) => prompt.push_str(&format!(
                "Current topic: {topic} (since {})\n",
                format_clock(since)
            )),
            None => prompt.push_str(&format!("Current topic: {topic}\n")),
        }
    }
    if !context.glossary.is_empty() {
        prompt.push_str(&format!(
            "Glossary (use these spellings): {}\n",
            context.glossary.join(", ")
        ));
    }
    if !prompt.is_empty() {
        prompt.push('\n');
    }

    let previous = context.previous;
    let recent = &previous[previous.len().saturating_sub(PREVIOUS_ENTRIES_IN_PROMPT)..];
    if !recent.is_empty() {
        // Grouped under their topics, as the panel shows them.
        prompt.push_str("Notes so far (do not repeat):\n");
        let mut shown_topic: Option<&str> = None;
        for entry in recent {
            if let Some(topic) = entry.topic.as_deref() {
                if shown_topic != Some(topic) {
                    prompt.push_str(&format!("Topic \"{topic}\":\n"));
                    shown_topic = Some(topic);
                }
            }
            prompt.push_str(&format!(
                "- [{}] ({}) {}\n",
                format_clock(entry.start_ms),
                entry.kind,
                entry.text
            ));
        }
        prompt.push('\n');
    }

    let carryover = carryover_segments(context.carryover, window.start_ms);
    if !carryover.is_empty() {
        prompt.push_str("Earlier transcript (context only, already covered):\n");
        for segment in &carryover {
            push_line(&mut prompt, segment);
        }
        prompt.push('\n');
    }

    prompt.push_str(&format!(
        "New transcript ({} to {}):\n",
        format_clock(window.start_ms),
        format_clock(window.end_ms)
    ));
    for segment in &window.segments {
        push_line(&mut prompt, segment);
    }

    let count = if window.max_entries <= 1 {
        "at most 1 note".to_string()
    } else {
        format!("up to {} notes (usually one)", window.max_entries)
    };
    prompt.push_str(&format!(
        "\nWrite {count} about the new transcript only, or {{\"entries\":[]}} if nothing new was said. JSON only."
    ));
    prompt
}

/// When the latest run of notes under `topic` started.
pub fn topic_since(previous: &[PreviousEntry], topic: &str) -> Option<i64> {
    previous
        .iter()
        .rev()
        .take_while(|e| e.topic.as_deref() == Some(topic))
        .last()
        .map(|e| e.start_ms)
}

/// Notes under the current topic the topic step shows.
const TOPIC_STEP_NOTES: usize = 3;

/// The topic step: one small question per window, before the notes, because
/// a small model asked for notes and a topic together keeps copying the
/// current topic long after the subject changed.
pub fn topic_system_prompt() -> &'static str {
    "You track the topic of a live meeting. A topic is the subject being discussed now: the \
company, product, project, person or agenda item the conversation is about, named in at most \
5 words.\n\
\n\
You get the current topic, the latest notes under it and the newest transcript. Decide whether \
the newest transcript is still mainly about the current topic.\n\
- Same subject, even with new details: {\"same\":true}\n\
- A different subject: {\"same\":false,\"topic\":\"New topic\"}\n\
\n\
Moving from someone's studies and past jobs to the project they work on now is a new topic. \
Moving from that project to another company is a new topic. A new detail, number or opinion \
about the same thing is not. Most meetings change topic every 4 to 8 minutes.\n\
\n\
Examples:\n\
Current topic \"Priya's background\"; transcript about the data platform Priya builds at Acme now: \
{\"same\":false,\"topic\":\"Acme data platform\"}\n\
Current topic \"Acme data platform\"; transcript about how many engineers the platform team has: \
{\"same\":true}\n\
Current topic \"Acme data platform\"; transcript pitching another startup, Globex: \
{\"same\":false,\"topic\":\"Globex pitch\"}\n\
\n\
Reply with JSON only."
}

/// The topic step's question, or `None` when there is no current topic yet
/// (the first notes name it).
pub fn topic_user_prompt(window: &Window, context: &PromptContext<'_>) -> Option<String> {
    let topic = context
        .current_topic
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    let since = topic_since(context.previous, topic);
    let mut prompt = format!("Current topic: \"{topic}\"");
    if let Some(since) = since {
        let minutes = (window.start_ms - since).max(0) / 60_000;
        prompt.push_str(&format!(" (since {}, {minutes} min)", format_clock(since)));
    }
    prompt.push('\n');
    let under: Vec<&PreviousEntry> = context
        .previous
        .iter()
        .rev()
        .take_while(|e| e.topic.as_deref() == Some(topic))
        .take(TOPIC_STEP_NOTES)
        .collect();
    if !under.is_empty() {
        prompt.push_str("Latest notes under it:\n");
        for entry in under.iter().rev() {
            prompt.push_str(&format!("- {}\n", entry.text));
        }
    }
    prompt.push_str(&format!(
        "\nNewest transcript ({} to {}):\n",
        format_clock(window.start_ms),
        format_clock(window.end_ms)
    ));
    for segment in &window.segments {
        push_line(&mut prompt, segment);
    }
    prompt.push_str(&format!(
        "\nIs the newest transcript still mainly about \"{topic}\"? JSON only."
    ));
    Some(prompt)
}

fn push_line(prompt: &mut String, segment: &Segment) {
    prompt.push_str(&format!(
        "[{}] {}: {}\n",
        format_clock(segment.start_ms),
        segment.speaker.label(),
        segment.text.trim()
    ));
}

/// The previous window's last `CARRYOVER_MS` (at most `CARRYOVER_MAX_WORDS`,
/// newest kept), before `before_ms`. Segments are kept whole, except that a
/// single long segment is cut to its last words.
pub fn carryover_segments(segments: &[Segment], before_ms: i64) -> Vec<Segment> {
    let Some(last_end) = segments
        .iter()
        .filter(|s| s.start_ms < before_ms)
        .map(|s| s.end_ms.max(s.start_ms))
        .max()
    else {
        return Vec::new();
    };
    let from = last_end - CARRYOVER_MS;
    let mut kept: Vec<Segment> = Vec::new();
    let mut words = 0;
    for segment in segments
        .iter()
        .rev()
        .filter(|s| s.start_ms < before_ms && s.end_ms.max(s.start_ms) > from)
    {
        let count = word_count(&segment.text);
        if words + count > CARRYOVER_MAX_WORDS {
            if kept.is_empty() {
                // One long segment: keep its last words.
                let tail: Vec<&str> = segment.text.split_whitespace().collect();
                let start = tail.len().saturating_sub(CARRYOVER_MAX_WORDS);
                kept.push(Segment {
                    text: format!("… {}", tail[start..].join(" ")),
                    ..segment.clone()
                });
            }
            break;
        }
        words += count;
        kept.push(segment.clone());
    }
    kept.reverse();
    kept
}

/// Earlier notes the compact prompt shows.
pub const COMPACT_PREVIOUS_NOTES: usize = 5;

/// The compact notetaker prompt: the same rules as [`system_prompt`] in a
/// third of the tokens, and a plain-text answer instead of JSON, so a small
/// model spends its few output tokens on the notes: a topic line (`same`, or
/// the new topic, which folds the topic step into the notes call) and one
/// `mm:ss|kind|owner|note` line per note. [`compact_grammar`] can enforce it.
pub fn compact_system_prompt() -> &'static str {
    "You take live meeting notes. You get the newest transcript and write short notes about what was said.\n\
\n\
Answer in this format and nothing else:\n\
topic: same\n\
mm:ss|kind|owner|note\n\
\n\
- First line: \"topic: same\" while the new transcript is still about the current topic, even with \
new details. When the subject changed, \"topic: \" and the new topic in at most 5 words: the \
company, product, project or person now discussed. Moving from someone's background to the \
project they work on now, or from that project to another company, is a new topic.\n\
- Then one line per note. kind: point, decision or action.\n\
- action: someone commits to do something: \"I'll send the deck\", an agreed \"let's\", an offer \
(\"I can introduce you to the founders\"), or a request You accept. owner: You, Others or the \
person's name. note: the task, starting with a verb.\n\
- decision: the participants explicitly agree on something. point: everything else (facts, \
plans described, opinions, questions). owner is - for points and decisions.\n\
- note: headline style, 6 to 12 words, never more than 14. Facts, names and numbers first. \
Say WHAT was said, never who said it: no \"Others\", \"Speaker\", \"They\", no \"discusses\" or \"mentions\".\n\
- Numbers only as said in the new transcript, never moved between companies or people.\n\
- Usually one note; a second only for a separate substantive fact. Skip small talk and \
anything already noted. If nothing new was said, write \"none\" after the topic line.\n\
\n\
Transcript labels: You is the person recording, Others is everyone else. Never put them in a note.\n\
\n\
Example:\n\
topic: Launch plan\n\
04:12|point|-|Offline sync bug is the last blocker; fix needs ~3 days\n\
04:40|action|You|send support the launch announcement today"
}

/// The compact user turn: topic, glossary, the last notes in the answer
/// format, the previous window's tail and the window's transcript.
pub fn compact_user_prompt(window: &Window, context: &PromptContext<'_>) -> String {
    let mut prompt = String::new();
    // The topic step's answer, when it ran: the model writes "topic: same".
    let decided = context.decided_topic.filter(|t| !t.trim().is_empty());
    match decided.or(context.current_topic.filter(|t| !t.trim().is_empty())) {
        Some(topic) if decided.is_some() => {
            prompt.push_str(&format!("Current topic: {} (decided)\n", topic.trim()))
        }
        Some(topic) => {
            let topic = topic.trim();
            match topic_since(context.previous, topic) {
                Some(since) if window.start_ms - since > LONG_TOPIC_MS => prompt.push_str(&format!(
                    "Current topic: {topic} (since {}, a long time: is the subject still the same?)\n",
                    format_clock(since)
                )),
                Some(since) => prompt.push_str(&format!(
                    "Current topic: {topic} (since {})\n",
                    format_clock(since)
                )),
                None => prompt.push_str(&format!("Current topic: {topic}\n")),
            }
        }
        None => prompt.push_str("Current topic: none yet (name it)\n"),
    }
    if !context.glossary.is_empty() {
        prompt.push_str(&format!("Spellings: {}\n", context.glossary.join(", ")));
    }
    let previous = context.previous;
    let recent = &previous[previous.len().saturating_sub(COMPACT_PREVIOUS_NOTES)..];
    if !recent.is_empty() {
        prompt.push_str("Notes so far (do not repeat):\n");
        let mut shown: Option<&str> = None;
        for entry in recent {
            if let Some(topic) = entry.topic.as_deref().filter(|t| Some(*t) != shown) {
                prompt.push_str(&format!("topic: {topic}\n"));
                shown = Some(topic);
            }
            prompt.push_str(&format!(
                "{}|{}|-|{}\n",
                format_clock(entry.start_ms),
                entry.kind,
                entry.text
            ));
        }
    }
    let carryover = carryover_segments(context.carryover, window.start_ms);
    if !carryover.is_empty() {
        prompt.push_str("Earlier transcript (already covered):\n");
        for segment in &carryover {
            push_line(&mut prompt, segment);
        }
    }
    prompt.push_str(&format!(
        "New transcript ({} to {}):\n",
        format_clock(window.start_ms),
        format_clock(window.end_ms)
    ));
    for segment in &window.segments {
        push_line(&mut prompt, segment);
    }
    let count = if window.max_entries <= 1 {
        "at most 1 note".to_string()
    } else {
        format!("up to {} notes (usually one)", window.max_entries)
    };
    prompt.push_str(&format!(
        "\nThe topic line, then {count} about the new transcript, or none."
    ));
    prompt
}

/// GBNF for the compact answer: the topic line, then `none` or one to
/// three note lines.
pub fn compact_grammar() -> &'static str {
    r#"root ::= "topic: " topic "\n" ("none" | line line? line?)
topic ::= [^|\n]{1,60}
line ::= clock "|" kind "|" owner "|" note "\n"
clock ::= ([0-9] ":")? [0-9] [0-9]? ":" [0-9] [0-9]
kind ::= "point" | "decision" | "action"
owner ::= "-" | [A-Z] [A-Za-z.' -]{0,30}
note ::= [^|\n]{3,200}
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_summary::scheduler::Speaker;
    use std::time::Duration;

    fn segment(start_ms: i64, speaker: Speaker, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms: start_ms + 4_000,
            speaker,
            text: text.into(),
            received_at: Duration::ZERO,
        }
    }

    fn window() -> Window {
        Window {
            segments: vec![
                segment(61_000, Speaker::Others, " We should ship on Friday. "),
                Segment {
                    end_ms: 70_000,
                    ..segment(66_500, Speaker::You, "I'll write the release notes.")
                },
            ],
            start_ms: 61_000,
            end_ms: 70_000,
            max_entries: 2,
            dropped_segments: 0,
        }
    }

    fn previous(n: usize) -> Vec<PreviousEntry> {
        (0..n)
            .map(|i| PreviousEntry {
                start_ms: i as i64 * 60_000,
                kind: "point".into(),
                text: format!("earlier entry {i}"),
                topic: Some(if i < 3 { "Budget" } else { "Hiring plan" }.into()),
            })
            .collect()
    }

    #[test]
    fn compact_prompt_keeps_the_rules_in_far_fewer_words() {
        let compact = compact_system_prompt();
        assert!(compact.len() * 2 < system_prompt().len());
        assert!(compact.contains("topic: same\nmm:ss|kind|owner|note"));
        assert!(compact.contains("never who said it"));
        assert!(compact.contains("never more than 14"));
        assert!(compact.contains("write \"none\" after the topic line"));
        assert!(compact.contains("|action|You|"));
    }

    #[test]
    fn compact_user_prompt_shows_notes_in_the_answer_format() {
        let previous = previous(7);
        let glossary = vec!["Acme".to_string()];
        let user = compact_user_prompt(
            &window(),
            &PromptContext {
                current_topic: Some("Hiring plan"),
                previous: &previous,
                glossary: &glossary,
                carryover: &[],
                decided_topic: None,
            },
        );
        assert!(user.contains("Current topic: Hiring plan (since 03:00)"));
        assert!(user.contains("Spellings: Acme"));
        assert!(user.contains("topic: Hiring plan\n03:00|point|-|earlier entry 3"));
        assert!(user.contains("06:00|point|-|earlier entry 6"));
        assert!(
            !user.contains("earlier entry 1"),
            "only the last {COMPACT_PREVIOUS_NOTES}"
        );
        assert!(user.contains("[01:01] Others: We should ship on Friday."));
        assert!(user.ends_with(
            "The topic line, then up to 2 notes (usually one) about the new transcript, or none."
        ));
        let first = compact_user_prompt(&window(), &PromptContext::default());
        assert!(first.starts_with("Current topic: none yet (name it)\n"));
    }

    #[test]
    fn compact_grammar_allows_none_or_up_to_three_lines() {
        let grammar = compact_grammar();
        assert!(
            grammar.starts_with("root ::= \"topic: \" topic \"\\n\" (\"none\" | line line? line?)")
        );
        assert!(grammar.contains("kind ::= \"point\" | \"decision\" | \"action\""));
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(format_clock(0), "00:00");
        assert_eq!(format_clock(61_900), "01:01");
        assert_eq!(format_clock(3_725_000), "1:02:05");
        assert_eq!(format_clock(-5), "00:00");
    }

    #[test]
    fn system_prompt_asks_for_content_not_narration() {
        let prompt = system_prompt();
        assert!(prompt.contains("State WHAT was said, never who said it"));
        assert!(prompt.contains("\"Others\""));
        assert!(prompt.contains("never more than 14"));
        assert!(prompt.contains("Usually one note per request"));
        assert!(prompt.contains("{\"entries\":[]}"));
        assert!(prompt.contains("That is a normal answer"));
        // The format example no longer suggests attributing notes to "Others".
        assert!(!prompt.contains("\"speaker\":\"Others\"}"));
        assert!(prompt.contains("\"speaker\":null}]}"));
    }

    #[test]
    fn system_prompt_defines_strict_kinds_and_topics() {
        let prompt = system_prompt();
        assert!(prompt.contains("a commitment or follow-up someone takes on"));
        assert!(prompt.contains("\"Others\" when the other side commits"));
        assert!(prompt.contains("suggestions nobody took on"));
        assert!(prompt.contains("explicitly agree"));
        assert!(prompt.contains("\"topic\" names what is being discussed NOW"));
        assert!(prompt.contains("is three topics"));
        assert!(prompt.contains("Numbers only from the new transcript's own lines"));
        // Two positive action examples, owned by each side.
        assert!(prompt.contains("\"speaker\":\"Others\",\"text\":\"introduce You"));
        assert!(prompt.contains("\"speaker\":\"You\",\"text\":\"connect with"));
        // Good and bad examples, and the bad ones say why.
        assert!(prompt.matches("Good: ").count() >= 2);
        assert!(prompt.matches("Bad: ").count() >= 2);
        assert!(prompt.contains("narrates who spoke"));
    }

    #[test]
    fn user_prompt_labels_lines_and_asks_for_up_to_n() {
        let prompt = user_prompt(&window(), &PromptContext::default());
        assert!(prompt.contains("[01:01] Others: We should ship on Friday.\n"));
        assert!(prompt.contains("[01:06] You: I'll write the release notes.\n"));
        assert!(prompt.contains("New transcript (01:01 to 01:10)"));
        assert!(prompt.contains("Write up to 2 notes"));
        assert!(prompt.contains("or {\"entries\":[]} if nothing new"));
        assert!(!prompt.contains("Return 1 to"));
        assert!(!prompt.contains("Notes so far"));
        assert!(!prompt.contains("Current topic"));
        assert!(!prompt.contains("Glossary"));
        assert!(!prompt.contains("context only"));
    }

    #[test]
    fn user_prompt_shows_the_last_eight_notes_with_their_topics() {
        let previous = previous(10);
        let context = PromptContext {
            previous: &previous,
            ..Default::default()
        };
        let prompt = user_prompt(&window(), &context);
        assert!(prompt.contains("Notes so far (do not repeat):"));
        assert!(!prompt.contains("earlier entry 1\n"));
        // Grouped under topic lines, each topic named once.
        assert!(prompt.contains(
            "Notes so far (do not repeat):\nTopic \"Budget\":\n- [02:00] (point) earlier entry 2\nTopic \"Hiring plan\":\n- [03:00] (point) earlier entry 3\n"
        ));
        assert!(prompt.contains("- [09:00] (point) earlier entry 9\n"));
        assert_eq!(prompt.matches("Topic \"Hiring plan\"").count(), 1);
        assert_eq!(prompt.matches("earlier entry").count(), 8);
    }

    #[test]
    fn the_current_topic_says_since_when_it_runs() {
        let previous = previous(10);
        let context = PromptContext {
            current_topic: Some("Hiring plan"),
            previous: &previous,
            ..Default::default()
        };
        let mut late = window();
        late.start_ms = 7 * 60_000;
        assert!(
            user_prompt(&late, &context).starts_with("Current topic: Hiring plan (since 03:00)\n")
        );
        // Running for over five minutes: offered, not imposed.
        late.start_ms = 9 * 60_000;
        assert!(user_prompt(&late, &context).starts_with(
            "Topic so far: Hiring plan (since 03:00). Name the topic of the new transcript afresh"
        ));
    }

    #[test]
    fn the_topic_step_asks_about_the_current_topic() {
        let previous = previous(10);
        let mut context = PromptContext {
            current_topic: Some("Hiring plan"),
            previous: &previous,
            ..Default::default()
        };
        let mut late = window();
        late.start_ms = 11 * 60_000;
        let question = topic_user_prompt(&late, &context).unwrap();
        assert!(question.starts_with("Current topic: \"Hiring plan\" (since 03:00, 8 min)\n"));
        // The last three notes under it, oldest first, and the transcript.
        assert!(question.contains(
            "Latest notes under it:\n- earlier entry 7\n- earlier entry 8\n- earlier entry 9\n"
        ));
        assert!(question.contains("[01:01] Others: We should ship on Friday.\n"));
        assert!(question.ends_with("still mainly about \"Hiring plan\"? JSON only."));
        assert!(topic_system_prompt().contains("{\"same\":false,\"topic\":\"New topic\"}"));
        // The notes prompt then states the decided topic instead.
        context.decided_topic = Some("Globex pitch");
        assert!(user_prompt(&late, &context)
            .starts_with("Topic of the new transcript: Globex pitch (already decided"));
        // No current topic: no step.
        context.current_topic = None;
        assert!(topic_user_prompt(&late, &context).is_none());
    }

    #[test]
    fn user_prompt_carries_topic_glossary_and_previous_context() {
        let glossary = vec!["Acme Robotics".to_string(), "Priya".to_string()];
        let carryover = vec![
            segment(20_000, Speaker::Others, "too old to show"),
            segment(52_000, Speaker::Others, "the budget is fine for now"),
        ];
        let context = PromptContext {
            current_topic: Some("Launch date"),
            glossary: &glossary,
            carryover: &carryover,
            ..Default::default()
        };
        let prompt = user_prompt(&window(), &context);
        assert!(prompt.starts_with("Current topic: Launch date\n"));
        assert!(prompt.contains("Glossary (use these spellings): Acme Robotics, Priya\n"));
        assert!(prompt.contains(
            "Earlier transcript (context only, already covered):\n[00:52] Others: the budget is fine for now\n"
        ));
        assert!(!prompt.contains("too old to show"));
        // Context comes before the new transcript.
        assert!(prompt.find("already covered").unwrap() < prompt.find("New transcript").unwrap());
    }

    #[test]
    fn a_single_entry_budget_is_worded_as_such() {
        let mut window = window();
        window.max_entries = 1;
        assert!(user_prompt(&window, &PromptContext::default()).contains("Write at most 1 note"));
    }

    #[test]
    fn carryover_keeps_the_last_thirty_seconds_and_caps_words() {
        let segments: Vec<Segment> = (0..10)
            .map(|i| segment(i * 10_000, Speaker::Others, &format!("line {i}")))
            .collect();
        let kept = carryover_segments(&segments, 200_000);
        // The last segment ends at 94 s: segments ending after 64 s are kept.
        let starts: Vec<i64> = kept.iter().map(|s| s.start_ms).collect();
        assert_eq!(starts, [70_000, 80_000, 90_000]);

        let long = vec![segment(0, Speaker::Others, &"word ".repeat(200))];
        let kept = carryover_segments(&long, 100_000);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].text.starts_with("… "));
        assert_eq!(word_count(&kept[0].text), CARRYOVER_MAX_WORDS);

        assert!(carryover_segments(&[], 0).is_empty());
        // Nothing before the new window: no context.
        assert!(carryover_segments(&segments, 0).is_empty());
    }
}
