//! Copilot's prompts. Pure. The system prompt and context come first and the
//! task last, so consecutive requests share the longest possible prefix.

use serde::{Deserialize, Serialize};

pub const SYSTEM_PROMPT: &str = "You are Copilot, a live assistant inside a meeting app. You help the person recording the meeting while the meeting is still happening.

Speaker labels:
- You: the person recording, on the microphone. Address them as \"you\".
- Others: the remote participants, from system audio. It can be several people.

You get the live summary so far, the transcript of the last few minutes as \"[MM:SS] Speaker: text\" lines, sometimes the user's own notes, and then a task.

Rules:
- Use only that material. If it does not cover what is asked, say plainly that it has not come up in the meeting so far. Never guess, and never invent names, numbers, dates or decisions.
- Cite the moments you rely on as [MM:SS], copied exactly from the transcript or summary lines.
- Be brief: short sentences, at most about 80 words, unless the task asks for a draft.
- Plain text or simple Markdown. No preamble, and do not restate the task.";

/// The phrase the model is told to use when something was never discussed.
pub const NOT_DISCUSSED_RULE: &str = "it has not come up in the meeting so far";

/// The quick actions on the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuickAction {
    WhatToSay,
    Explain,
    CatchUp,
    FollowUp,
    Shorter,
}

impl QuickAction {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "what_to_say" => Some(Self::WhatToSay),
            "explain" => Some(Self::Explain),
            "catch_up" => Some(Self::CatchUp),
            "follow_up" => Some(Self::FollowUp),
            "shorter" => Some(Self::Shorter),
            _ => None,
        }
    }

    /// The label shown on the pill and saved with the moment.
    pub fn label(self) -> &'static str {
        match self {
            Self::WhatToSay => "What should I say?",
            Self::Explain => "Explain that",
            Self::CatchUp => "Catch me up",
            Self::FollowUp => "Draft follow-up",
            Self::Shorter => "Shorter",
        }
    }

    fn instruction(self) -> &'static str {
        match self {
            Self::WhatToSay => "Suggest what You could say next, given the last few minutes. Give one to three short options You could say out loud, one sentence each, as a list.",
            Self::Explain => "Explain the most recent topic, term or point Others raised, in plain words, in two to four sentences.",
            Self::CatchUp => "Catch You up: what has happened in this meeting so far, focusing on the last few minutes. Up to five short bullets, each citing [MM:SS].",
            Self::FollowUp => "Draft a short follow-up message You could send after this meeting: the decisions, the action items with owners, and open questions. Under 120 words.",
            Self::Shorter => "Rewrite the answer below to be much shorter, one or two sentences, keeping its [MM:SS] citations.",
        }
    }
}

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    Action(QuickAction),
    /// "Shorter": the previous answer to shorten.
    Shorten {
        previous: String,
    },
    Question(String),
    /// Is the line at `at_ms` a question for You? One word.
    Confirm {
        at_ms: i64,
        line: String,
    },
    /// Draft a reply to the line at `at_ms`.
    Suggest {
        at_ms: i64,
        line: String,
    },
}

/// Longest question or quoted line put into a task.
pub const MAX_TASK_CHARS: usize = 800;

fn clip(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// `MM:SS`, or `H:MM:SS` past the hour.
pub fn format_time(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

impl Task {
    pub fn text(&self) -> String {
        match self {
            Task::Action(action) => action.instruction().to_string(),
            Task::Shorten { previous } => format!(
                "{}\n\nAnswer to shorten:\n{}",
                QuickAction::Shorter.instruction(),
                clip(previous, 2000)
            ),
            Task::Question(question) => format!(
                "Answer this question from You: {}",
                clip(question, MAX_TASK_CHARS)
            ),
            Task::Confirm { at_ms, line } => format!(
                "The last line from Others, at [{}], is: \"{}\"\nIs it a question or a request that You, the person recording, should answer now? Reply with exactly one word: yes or no.",
                format_time(*at_ms),
                clip(line, MAX_TASK_CHARS)
            ),
            Task::Suggest { at_ms, line } => format!(
                "Others just said, at [{}]: \"{}\"\nDraft what You could say in reply: one to three sentences in the first person, ready to say out loud. If the answer depends on facts that are not in the meeting, say what You would need to check instead of inventing them.",
                format_time(*at_ms),
                clip(line, MAX_TASK_CHARS)
            ),
        }
    }

    /// Output budget for this task.
    pub fn max_tokens(&self) -> u32 {
        match self {
            Task::Confirm { .. } => 5,
            Task::Shorten { .. } => 150,
            Task::Action(QuickAction::FollowUp) => 300,
            _ => 300,
        }
    }

    /// Sampling temperature: deterministic for the yes/no gate.
    pub fn temperature(&self) -> f32 {
        match self {
            Task::Confirm { .. } => 0.0,
            _ => 0.3,
        }
    }
}

/// Drops `<think>…</think>` blocks (and an unclosed one) from a reply.
pub fn strip_reasoning(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_prompt_explains_the_labels_citations_and_honesty() {
        assert!(SYSTEM_PROMPT.contains("You: the person recording, on the microphone"));
        assert!(SYSTEM_PROMPT.contains("Others: the remote participants, from system audio"));
        assert!(SYSTEM_PROMPT.contains("[MM:SS]"));
        assert!(
            SYSTEM_PROMPT.contains(NOT_DISCUSSED_RULE),
            "the model must be told to say when something was never discussed"
        );
        assert!(SYSTEM_PROMPT.contains("Never guess"));
    }

    #[test]
    fn tasks_have_sane_budgets() {
        let confirm = Task::Confirm {
            at_ms: 61_000,
            line: "Can you send it?".into(),
        };
        assert_eq!(confirm.max_tokens(), 5);
        assert_eq!(confirm.temperature(), 0.0);
        assert!(confirm.text().contains("[01:01]"));
        assert!(confirm.text().contains("yes or no"));
        assert_eq!(Task::Action(QuickAction::WhatToSay).max_tokens(), 300);
        assert!((Task::Question("x".into()).temperature() - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn quick_actions_round_trip_and_label() {
        for (id, label) in [
            ("what_to_say", "What should I say?"),
            ("explain", "Explain that"),
            ("catch_up", "Catch me up"),
            ("follow_up", "Draft follow-up"),
        ] {
            assert_eq!(QuickAction::parse(id).unwrap().label(), label);
        }
        assert!(QuickAction::parse("nope").is_none());
    }

    #[test]
    fn long_questions_are_clipped() {
        let text = Task::Question("a".repeat(5000)).text();
        assert!(text.chars().count() < MAX_TASK_CHARS + 60);
    }

    #[test]
    fn times_format_like_the_transcript() {
        assert_eq!(format_time(0), "00:00");
        assert_eq!(format_time(1_531_000), "25:31");
        assert_eq!(format_time(3_725_000), "1:02:05");
    }

    #[test]
    fn reasoning_is_stripped() {
        assert_eq!(strip_reasoning("<think>x</think>\nHello"), "Hello");
        assert_eq!(strip_reasoning("Hi <think>unfinished"), "Hi");
        assert_eq!(strip_reasoning("plain"), "plain");
    }
}
