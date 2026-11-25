//! The meeting title: the prompt of the title call, and cleaning its answer
//! into one short line.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::summary::markdown::{clean_llm_markdown_detailed, contains_reasoning_marker};

/// Keeps the title prompt small enough for local models' context windows.
const TITLE_TRANSCRIPT_MAX_CHARS: usize = 12_000;
const TITLE_MAX_WORDS: usize = 8;
const TITLE_MAX_CHARS: usize = 80;

static TITLE_LABEL_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(?:meeting\s+)?title\s*:").unwrap());

/// System prompt for the standalone title call. It deliberately carries no
/// example title, since small models echo examples instead of reading the transcript.
pub fn title_system_prompt() -> &'static str {
    r#"You name meetings. Read the transcript, and the user's notes if any, and reply with ONLY a short title of at most 6 words describing what this specific meeting discussed: its main topic, decision, or project, using the actual names and subjects from the conversation.

Do not use a generic or placeholder title. Do not add quotes, Markdown, a "Title:" label, trailing punctuation, reasoning, or any explanation. Output ONLY the title on a single line."#
}

pub fn build_title_user_prompt(transcript: &str, notes: Option<&str>) -> String {
    let transcript = transcript.trim();
    let transcript = match transcript.char_indices().nth(TITLE_TRANSCRIPT_MAX_CHARS) {
        Some((cut, _)) => &transcript[..cut],
        None => transcript,
    };
    let mut prompt = String::from("Write a short title (max 6 words) for this meeting.\n\n");
    if let Some(notes) = notes.map(str::trim).filter(|notes| !notes.is_empty()) {
        prompt.push_str(&format!("<my_notes>\n{notes}\n</my_notes>\n\n"));
    }
    prompt.push_str(&format!(
        "<transcript>\n{transcript}\n</transcript>\n\nReturn ONLY the title."
    ));
    prompt
}

/// Turns a raw title completion into a clean single-line title, or None when
/// nothing usable is left.
pub fn sanitize_generated_title(raw: &str) -> Option<String> {
    let cleaned = clean_llm_markdown_detailed(raw);
    if contains_reasoning_marker(&cleaned.markdown) {
        return None;
    }
    cleaned
        .markdown
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .find_map(sanitize_title_line)
}

fn sanitize_title_line(line: &str) -> Option<String> {
    const WRAPPERS: &[char] = &['"', '`', '*', '_', '“', '”'];
    let mut title = line.trim();
    // Decorations nest in any order (`**Title:** "X"`), so strip until stable.
    loop {
        let before = title;
        title = title.trim_start_matches('#').trim();
        title = title.trim_matches(WRAPPERS).trim();
        for (open, close) in [('\'', '\''), ('‘', '’')] {
            if title.len() > 1 && title.starts_with(open) && title.ends_with(close) {
                title = title[open.len_utf8()..title.len() - close.len_utf8()].trim();
            }
        }
        if let Some(label) = TITLE_LABEL_REGEX.find(title) {
            title = title[label.end()..].trim();
        }
        if title == before {
            break;
        }
    }

    let title = title
        .split_whitespace()
        .take(TITLE_MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    // Over the character cap, cut at the last word boundary that fits.
    let title = if title.chars().count() > TITLE_MAX_CHARS {
        let cut: String = title.chars().take(TITLE_MAX_CHARS + 1).collect();
        match cut.rfind(' ') {
            Some(end) => cut[..end].to_string(),
            None => cut.chars().take(TITLE_MAX_CHARS).collect(),
        }
    } else {
        title
    };
    let title = title
        .trim_end_matches(['.', ',', ';', ':', '!', '…', '-'])
        .trim_end();
    (!title.is_empty()).then(|| title.to_string())
}

/// Extracts meeting name from the first heading in markdown
///
/// # Arguments
/// * `markdown` - Markdown content
///
/// # Returns
/// Meeting name if found, None otherwise
pub fn extract_meeting_name_from_markdown(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_prompt_carries_no_example_title() {
        // Small models copy an example title instead of naming the meeting.
        assert!(!title_system_prompt().contains("e.g."));
    }

    #[test]
    fn title_user_prompt_wraps_transcript_and_non_blank_notes() {
        let prompt = build_title_user_prompt("Alice: ship it", Some("  launch date?  "));
        assert!(prompt.contains("<transcript>\nAlice: ship it\n</transcript>"));
        assert!(prompt.contains("<my_notes>\nlaunch date?\n</my_notes>"));

        for notes in [None, Some(""), Some("  \n\t")] {
            let prompt = build_title_user_prompt("Alice: ship it", notes);
            assert!(!prompt.contains("<my_notes>"));
            assert!(prompt.contains("<transcript>"));
        }
    }

    #[test]
    fn title_user_prompt_truncates_long_transcript_on_char_boundary() {
        let transcript = "é".repeat(TITLE_TRANSCRIPT_MAX_CHARS + 500);
        let prompt = build_title_user_prompt(&transcript, None);
        let kept = prompt
            .split("<transcript>\n")
            .nth(1)
            .and_then(|rest| rest.split("\n</transcript>").next())
            .unwrap();
        assert_eq!(kept.chars().count(), TITLE_TRANSCRIPT_MAX_CHARS);
    }

    #[test]
    fn sanitizer_extracts_clean_title() {
        for (raw, expected) in [
            ("Budget Review", "Budget Review"),
            (
                "<think>they talk about hiring</think>\nHiring Plan",
                "Hiring Plan",
            ),
            ("# Q3 Launch Planning", "Q3 Launch Planning"),
            ("## Q3 Launch Planning\n\nbody", "Q3 Launch Planning"),
            ("\"Vendor Contract Renewal\"", "Vendor Contract Renewal"),
            ("'Vendor Contract Renewal'", "Vendor Contract Renewal"),
            ("“Vendor Contract Renewal”", "Vendor Contract Renewal"),
            ("`Vendor Contract Renewal`", "Vendor Contract Renewal"),
            (
                "Title: Onboarding Flow Redesign",
                "Onboarding Flow Redesign",
            ),
            (
                "**Title:** \"Onboarding Flow Redesign.\"",
                "Onboarding Flow Redesign",
            ),
            ("Meeting title: Onboarding Flow", "Onboarding Flow"),
            (
                "```\nDatabase Migration Plan\n```",
                "Database Migration Plan",
            ),
            (
                "```text\nDatabase Migration Plan\n```",
                "Database Migration Plan",
            ),
            (
                "\n\n  Weekly   Sync\twith  Design.  \nextra line",
                "Weekly Sync with Design",
            ),
            ("Hire or Build?", "Hire or Build?"),
        ] {
            assert_eq!(
                sanitize_generated_title(raw).as_deref(),
                Some(expected),
                "raw: {raw:?}"
            );
        }
    }

    #[test]
    fn sanitizer_rejects_empty_and_reasoning_only_output() {
        for raw in [
            "",
            "   \n\t",
            "<think>only thinking</think>",
            "```\n```",
            "#",
            "\"\"",
            "Visible\n<think>unterminated",
        ] {
            assert_eq!(sanitize_generated_title(raw), None, "raw: {raw:?}");
        }
    }

    #[test]
    fn sanitizer_caps_long_titles() {
        let title = sanitize_generated_title(
            "One two three four five six seven eight nine ten eleven twelve.",
        )
        .unwrap();
        assert_eq!(title, "One two three four five six seven eight");

        let title =
            sanitize_generated_title(&"word ".repeat(3).replace("word", &"x".repeat(40))).unwrap();
        assert!(title.chars().count() <= TITLE_MAX_CHARS);
        assert_eq!(title.split_whitespace().count(), 1);

        let title = sanitize_generated_title(&"é".repeat(200)).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);

        let title =
            sanitize_generated_title("Alpha, beta, gamma, delta, epsilon, zeta, eta, theta, iota")
                .unwrap();
        assert_eq!(
            title,
            "Alpha, beta, gamma, delta, epsilon, zeta, eta, theta"
        );
    }
}
