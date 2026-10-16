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
- \"decision\": something the group agreed or decided.\n\
- \"action\": someone committed to do something (say who, if known).\n\
- \"point\": any other notable topic, fact or question.\n\
\n\
Rules:\n\
- Each \"text\" is at most 15 words, plain language, no filler.\n\
- Only cover the new transcript. Never repeat or rephrase an entry listed as already noted.\n\
- \"start\" and \"end\" are mm:ss times copied from the transcript lines the entry covers.\n\
- \"speaker\" is \"You\", \"Others\" or null.\n\
- If nothing new or meaningful was said, return {\"entries\":[]}.\n\
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
