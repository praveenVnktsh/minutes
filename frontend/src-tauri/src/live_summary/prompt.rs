//! The prompts for one live summary window. Pure.

use super::scheduler::Window;

/// How many earlier entries the prompt shows so the model does not repeat them.
pub const PREVIOUS_ENTRIES_IN_PROMPT: usize = 3;

/// An earlier entry, as the prompt shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviousEntry {
    pub start_ms: i64,
    pub kind: String,
    pub text: String,
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
    "You keep a live timeline of a meeting while it is being recorded. \
Each request gives you the newest part of the transcript. Write very short timeline entries for it.\n\
\n\
Speaker labels:\n\
- \"You\" is the person recording this meeting (their microphone).\n\
- \"Others\" is everyone else on the call (system audio). It may be several different people.\n\
\n\
Entry kinds:\n\
- \"action\": a concrete follow-up that someone in this meeting committed to do, such as sending, booking or checking something. \
Goals, plans, opinions and strategy are not actions, even when they mention the future.\n\
- \"decision\": something the people in this meeting agreed or decided.\n\
- \"point\": any other fact, number, name or question worth remembering.\n\
\n\
Writing \"text\":\n\
- Write the point itself. The speaker is shown next to the entry, so never say who spoke: \
no \"Others discuss\", \"Speaker explains\", \"You suggest\", \"They note\".\n\
- At most 12 words, written like a note. Keep every entry in the same plain style.\n\
- Prefer specifics: names, numbers, dates. Leave out vague words such as \"interesting\", \"exciting\", \"rapidly\".\n\
- Spell names as the transcript does; if a name was already noted, reuse the spelling from the entries already noted.\n\
- One entry per point. Never repeat or rephrase an entry listed as already noted.\n\
\n\
Fields:\n\
- \"start\" and \"end\" are mm:ss times copied from the transcript lines the entry covers.\n\
- \"speaker\" is the label of those lines, \"You\" or \"Others\", or null.\n\
- If nothing new or meaningful was said, return {\"entries\":[]}.\n\
\n\
Examples:\n\
- \"Others: we've grown from about forty to almost a hundred people this year\" is \
{\"kind\":\"point\",\"text\":\"Team grew from about 40 to almost 100 people this year.\"}, \
not \"Others discuss rapid team growth.\"\n\
- \"Others: the plan is to open a Berlin office next spring\" is \
{\"kind\":\"point\",\"text\":\"Plan: Berlin office next spring.\"}: a plan, not an action.\n\
- \"You: I'll send Lena the revised budget by Friday\" is \
{\"kind\":\"action\",\"text\":\"Send Lena the revised budget by Friday.\"}\n\
\n\
Reply with JSON only, no other text:\n\
{\"entries\":[{\"start\":\"mm:ss\",\"end\":\"mm:ss\",\"kind\":\"point\",\"text\":\"...\",\"speaker\":\"Others\"}]}"
}

/// The user turn: earlier entries not to repeat, the window's transcript,
/// and how many entries to return.
pub fn user_prompt(window: &Window, previous: &[PreviousEntry]) -> String {
    let mut prompt = String::new();

    let recent = &previous[previous.len().saturating_sub(PREVIOUS_ENTRIES_IN_PROMPT)..];
    if !recent.is_empty() {
        prompt.push_str("Already noted (do not repeat):\n");
        for entry in recent {
            prompt.push_str(&format!(
                "- [{}] ({}) {}\n",
                format_clock(entry.start_ms),
                entry.kind,
                entry.text
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str(&format!(
        "New transcript ({} to {}):\n",
        format_clock(window.start_ms),
        format_clock(window.end_ms)
    ));
    for segment in &window.segments {
        prompt.push_str(&format!(
            "[{}] {}: {}\n",
            format_clock(segment.start_ms),
            segment.speaker.label(),
            segment.text.trim()
        ));
    }

    let count = if window.max_entries <= 1 {
        "at most 1 entry".to_string()
    } else {
        format!("1 to {} entries", window.max_entries)
    };
    prompt.push_str(&format!(
        "\nReturn {count} for the new transcript as JSON only."
    ));
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_summary::scheduler::{Segment, Speaker};
    use std::time::Duration;

    fn window() -> Window {
        Window {
            segments: vec![
                Segment {
                    start_ms: 61_000,
                    end_ms: 65_000,
                    speaker: Speaker::Others,
                    text: " We should ship on Friday. ".into(),
                    received_at: Duration::ZERO,
                },
                Segment {
                    start_ms: 66_500,
                    end_ms: 70_000,
                    speaker: Speaker::You,
                    text: "I'll write the release notes.".into(),
                    received_at: Duration::ZERO,
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
            })
            .collect()
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(format_clock(0), "00:00");
        assert_eq!(format_clock(61_900), "01:01");
        assert_eq!(format_clock(3_725_000), "1:02:05");
        assert_eq!(format_clock(-5), "00:00");
    }

    #[test]
    fn system_prompt_explains_the_speakers_and_the_format() {
        let prompt = system_prompt();
        assert!(prompt.contains("\"You\" is the person recording"));
        assert!(prompt.contains("\"Others\" is everyone else"));
        assert!(prompt.contains("several different people"));
        assert!(prompt.contains("{\"entries\":[]}"));
        assert!(prompt.contains("decision"));
        assert!(prompt.contains("action"));
    }

    #[test]
    fn system_prompt_asks_for_the_point_not_who_said_it() {
        let prompt = system_prompt();
        assert!(prompt.contains("never say who spoke"));
        assert!(prompt.contains("\"Others discuss\""));
        assert!(prompt.contains("At most 12 words"));
    }

    #[test]
    fn system_prompt_keeps_actions_to_follow_ups_from_this_meeting() {
        let prompt = system_prompt();
        assert!(
            prompt.contains("a concrete follow-up that someone in this meeting committed to do")
        );
        assert!(prompt.contains("Goals, plans, opinions and strategy are not actions"));
    }

    #[test]
    fn system_prompt_keeps_names_spelled_as_already_noted() {
        assert!(system_prompt().contains("reuse the spelling from the entries already noted"));
    }

    #[test]
    fn system_prompt_examples_never_name_the_speaker_in_the_text() {
        let prompt = system_prompt();
        let examples: Vec<&str> = prompt
            .split("\"text\":\"")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap())
            .filter(|text| !text.is_empty() && *text != "...")
            .collect();
        assert!(examples.len() >= 3, "{examples:?}");
        for text in examples {
            for subject in ["You ", "Others ", "Speaker ", "They "] {
                assert!(!text.starts_with(subject), "{text}");
            }
            assert!(text.split_whitespace().count() <= 12, "{text}");
        }
    }

    #[test]
    fn user_prompt_labels_lines_with_time_and_speaker() {
        let prompt = user_prompt(&window(), &[]);
        assert!(prompt.contains("[01:01] Others: We should ship on Friday.\n"));
        assert!(prompt.contains("[01:06] You: I'll write the release notes.\n"));
        assert!(prompt.contains("New transcript (01:01 to 01:10)"));
        assert!(prompt.contains("Return 1 to 2 entries"));
        assert!(!prompt.contains("Already noted"));
    }

    #[test]
    fn user_prompt_shows_only_the_last_three_entries_not_to_repeat() {
        let prompt = user_prompt(&window(), &previous(5));
        assert!(prompt.contains("Already noted (do not repeat):"));
        assert!(!prompt.contains("earlier entry 1"));
        assert!(prompt.contains("- [02:00] (point) earlier entry 2"));
        assert!(prompt.contains("earlier entry 4"));
    }

    #[test]
    fn a_single_entry_budget_is_worded_as_such() {
        let mut window = window();
        window.max_entries = 1;
        assert!(user_prompt(&window, &[]).contains("Return at most 1 entry"));
    }
}
