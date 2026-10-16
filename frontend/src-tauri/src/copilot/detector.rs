//! The cheap first gate for proactive suggestions: does a finalized line from
//! Others look like a question or request aimed at the user? Pure, so the
//! table of examples below pins its behaviour. A pass is only a candidate:
//! a tiny model call confirms it before any card is shown.

/// The rule check's verdict, with a short reason for the debug line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleDecision {
    Pass(&'static str),
    Reject(&'static str),
}

impl RuleDecision {
    pub fn passed(self) -> bool {
        matches!(self, RuleDecision::Pass(_))
    }

    pub fn reason(self) -> &'static str {
        match self {
            RuleDecision::Pass(reason) | RuleDecision::Reject(reason) => reason,
        }
    }
}

/// Phrases that look like they address the user but are filler.
const FILLER_PHRASES: &[&str] = &[
    "you know what i mean",
    "you know what",
    "you know",
    "i mean",
    "thank you",
    "thanks to you",
    "see you",
    "mind you",
    "if you will",
    "you see",
];

/// Tag questions that turn a statement into a "question" nobody must answer.
const TAG_QUESTIONS: &[&str] = &[
    "right",
    "you know",
    "okay",
    "ok",
    "yeah",
    "yes",
    "no",
    "huh",
    "eh",
    "isn't it",
    "aren't we",
    "don't you think",
    "make sense",
    "makes sense",
    "does that make sense",
    "sound good",
    "sounds good",
    "you see",
    "correct",
    "agreed",
    "fair",
    "true",
];

/// Words a line can open with before the real start.
const LEADING_FILLERS: &[&str] = &[
    "so", "and", "but", "okay", "ok", "um", "uh", "well", "alright", "yeah", "right", "hey",
    "also", "oh", "now",
];

const INTERROGATIVES: &[&str] = &[
    "what", "what's", "whats", "why", "how", "how's", "when", "where", "who", "who's", "which",
    "whose", "can", "could", "would", "will", "do", "does", "did", "is", "are", "was", "were",
    "should", "shall", "have", "has", "may", "might", "any", "anything", "isn't", "aren't",
    "don't", "doesn't", "won't", "wouldn't", "couldn't",
];

/// Requests aimed at the listener even without a question mark.
const REQUEST_PHRASES: &[&str] = &[
    "can you",
    "could you",
    "would you",
    "will you",
    "do you",
    "did you",
    "are you",
    "have you",
    "were you",
    "your thoughts",
    "your take",
    "your input",
    "your view",
    "your opinion",
    "what about you",
    "how about you",
    "over to you",
    "i'd love to hear",
    "walk us through",
    "tell us",
    "let us know",
    "let me know",
];

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

/// Lowercase, curly quotes straightened, whitespace collapsed.
fn normalise(text: &str) -> String {
    let lowered = text
        .to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .replace(['…'], ".");
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Removes filler phrases on word boundaries.
fn strip_phrases(text: &str, phrases: &[&str]) -> String {
    let mut padded = format!(" {} ", words(text).join(" "));
    for phrase in phrases {
        let needle = format!(" {phrase} ");
        while let Some(at) = padded.find(&needle) {
            padded.replace_range(at..at + needle.len(), " ");
        }
    }
    padded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The last sentence, where a question sits in a multi-sentence line.
fn last_sentence(text: &str) -> &str {
    let trimmed = text.trim_end_matches(|c: char| c.is_whitespace() || c == '"');
    let body = trimmed.trim_end_matches(['?', '.', '!']);
    let start = body.rfind(['.', '!', '?']).map(|at| at + 1).unwrap_or(0);
    trimmed[start..].trim()
}

/// If the sentence ends in a tag question ("…, right?"), the statement before
/// it; `None` if the whole sentence is a tag.
fn without_tag(sentence: &str) -> Option<String> {
    let body = sentence.trim_end_matches(['?', '.', '!', ' ']);
    let lowered = words(body).join(" ");
    for tag in TAG_QUESTIONS {
        if lowered == *tag {
            return None;
        }
        if lowered.ends_with(&format!(" {tag}")) {
            let kept = lowered[..lowered.len() - tag.len() - 1].trim().to_string();
            return Some(kept);
        }
    }
    Some(lowered)
}

/// Whether a finalized line from Others is worth confirming with the model.
pub fn rule_check(text: &str) -> RuleDecision {
    let normalised = normalise(text);
    let all_words = words(&normalised);
    if all_words.is_empty() {
        return RuleDecision::Reject("empty");
    }
    let sentence = last_sentence(&normalised);
    let asks = sentence.ends_with('?');

    let Some(statement) = without_tag(sentence) else {
        return RuleDecision::Reject("filler question");
    };
    let had_tag = statement != words(sentence).join(" ");
    let content = strip_phrases(&statement, FILLER_PHRASES);
    let content_words = words(&content);
    let mentions_you = content_words
        .iter()
        .any(|w| matches!(w.as_str(), "you" | "your" | "you're" | "you'll" | "yours"));

    if content_words.is_empty() {
        return RuleDecision::Reject("filler");
    }
    if all_words.len() < 3 && !(asks && mentions_you) {
        return RuleDecision::Reject("too short");
    }
    if asks && !had_tag {
        return RuleDecision::Pass("question mark");
    }

    let first_real = content_words
        .iter()
        .find(|w| !LEADING_FILLERS.contains(&w.as_str()));
    if let Some(first) = first_real {
        if INTERROGATIVES.contains(&first.as_str()) && content_words.len() >= 3 {
            return RuleDecision::Pass("interrogative start");
        }
    }
    let padded = format!(" {content} ");
    if REQUEST_PHRASES
        .iter()
        .any(|phrase| padded.contains(&format!(" {phrase} ")))
    {
        return RuleDecision::Pass("request to you");
    }
    if mentions_you {
        return RuleDecision::Pass("mentions you");
    }
    if had_tag {
        return RuleDecision::Reject("tag question");
    }
    RuleDecision::Reject("statement")
}

/// Reads the confirm call's one-word answer. Anything but a clear yes is no.
pub fn parse_yes_no(reply: &str) -> bool {
    let cleaned = super::prompt::strip_reasoning(reply);
    words(&cleaned.to_lowercase())
        .first()
        .is_some_and(|w| w == "yes" || w == "y")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_and_requests_to_the_user_pass() {
        for line in [
            "What do you think about the pricing change?",
            "Can you send me the deck after this?",
            "how long would the migration take on your side",
            "Are we still on track for Friday",
            "So what's your take on the hiring plan?",
            "Could you walk us through the numbers",
            "Do you have the latest churn report?",
            "And you?",
            "I'd love your input on the roadmap.",
            "When can your team start on the integration?",
            "Okay, so, any concerns from your side?",
            "We looked at three vendors. Which one would you pick?",
            "Is the SOC 2 audit still scheduled for March?",
            "Let me know if the timeline works for you.",
        ] {
            let decision = rule_check(line);
            assert!(decision.passed(), "{line:?} should pass, got {decision:?}");
        }
    }

    #[test]
    fn fillers_tags_and_statements_are_rejected() {
        for line in [
            "right?",
            "You know?",
            "We should ship it on Friday, right?",
            "It was a good quarter, you know?",
            "Okay.",
            "Yeah, makes sense.",
            "So, um, yeah.",
            "Thank you so much.",
            "See you next week.",
            "The numbers look fine to me.",
            "Does that make sense?",
            "I mean, you know, it's fine.",
            "Sounds good?",
            "",
            "We agreed to move the launch to Q3, okay?",
        ] {
            let decision = rule_check(line);
            assert!(
                !decision.passed(),
                "{line:?} should be rejected, got {decision:?}"
            );
        }
    }

    #[test]
    fn the_confirm_reply_is_yes_only_when_it_says_yes() {
        assert!(parse_yes_no("Yes"));
        assert!(parse_yes_no(" yes."));
        assert!(parse_yes_no("<think>hmm</think>\nYes, they asked you."));
        assert!(!parse_yes_no("No"));
        assert!(!parse_yes_no("Maybe"));
        assert!(!parse_yes_no(""));
        assert!(!parse_yes_no("yesterday"));
    }
}
