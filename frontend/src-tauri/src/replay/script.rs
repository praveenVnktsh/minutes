//! Scripted Copilot actions for a replay.
//!
//! One step per line, `#` starts a comment:
//!
//! ```text
//! 05:00  ask     [honesty] What did they say about the budget for the Mars office?
//! 20:00  action  what_to_say
//! 75%    ask     [decision] What did we decide about {topic}?
//! ```
//!
//! - The time is `MM:SS`, `H:MM:SS` or a percentage of the recording.
//! - `ask` sends the rest of the line as a question; `action` runs a quick
//!   action (`what_to_say`, `explain`, `catch_up`, `follow_up`).
//! - An optional `[tag]` names the step in the report. `honesty` marks a
//!   question about something never discussed: the answer should say so.
//! - `{topic}` is filled in when the step runs, from the live summary so far
//!   (the latest decision, else action, else point), so a script can ask about
//!   a real topic of a private meeting without anyone reading it first.

use anyhow::{anyhow, bail, Result};

use super::events::parse_clock;

/// What the replay runs when no `--script` is given.
pub const DEFAULT_SCRIPT: &str = "\
# Never discussed: the answer should say it has not come up.
05:00  ask     [honesty] What did they say about the budget for the Mars office move?
20:00  action  [what-to-say] what_to_say
45%    action  [catch-up] catch_up
75%    ask     [decision] What did we decide about {topic}?
76%    ask     [repeat] Who owns the next step on {topic}?
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Ms(u64),
    /// Per mille of the recording, so `75.5%` works.
    PerMille(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Ask,
    Action,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Ask => "ask",
            StepKind::Action => "action",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub at: At,
    pub kind: StepKind,
    pub tag: Option<String>,
    pub text: String,
}

/// A step placed on the recording's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub at_ms: u64,
    pub step: Step,
}

const ACTIONS: &[&str] = &["what_to_say", "explain", "catch_up", "follow_up"];

pub fn parse(text: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let step = parse_line(line).map_err(|e| anyhow!("script line {}: {e}", index + 1))?;
        steps.push(step);
    }
    Ok(steps)
}

fn parse_line(line: &str) -> Result<Step> {
    let (time, after_time) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let after_time = after_time.trim_start();
    let (kind, rest) = after_time
        .split_once(char::is_whitespace)
        .unwrap_or((after_time, ""));
    let mut rest = rest.trim().to_string();

    let at = if let Some(percent) = time.strip_suffix('%') {
        let value: f64 = percent
            .parse()
            .map_err(|_| anyhow!("bad percentage {time:?}"))?;
        if !(0.0..=100.0).contains(&value) {
            bail!("percentage {time:?} is outside 0–100");
        }
        At::PerMille((value * 10.0).round() as u32)
    } else {
        At::Ms(parse_clock(time).ok_or_else(|| anyhow!("bad time {time:?}"))? * 1000)
    };
    let kind = match kind {
        "ask" => StepKind::Ask,
        "action" => StepKind::Action,
        other => bail!("unknown step {other:?} (ask or action)"),
    };
    let mut tag = None;
    if rest.starts_with('[') {
        if let Some(end) = rest.find(']') {
            tag = Some(rest[1..end].trim().to_string()).filter(|t| !t.is_empty());
            rest = rest[end + 1..].trim().to_string();
        }
    }
    if rest.is_empty() {
        bail!("a {} step needs text", kind.label());
    }
    if kind == StepKind::Action && !ACTIONS.contains(&rest.as_str()) {
        bail!("unknown quick action {rest:?} ({})", ACTIONS.join(", "));
    }
    Ok(Step {
        at,
        kind,
        tag,
        text: rest,
    })
}

/// Place steps on a recording of `duration_ms`, in time order. Steps past the
/// end are dropped and returned separately so the report can say so.
pub fn schedule(steps: &[Step], duration_ms: u64) -> (Vec<Scheduled>, Vec<Step>) {
    let mut placed = Vec::new();
    let mut dropped = Vec::new();
    for step in steps {
        let at_ms = match step.at {
            At::Ms(ms) => ms,
            At::PerMille(pm) => duration_ms * u64::from(pm) / 1000,
        };
        if at_ms >= duration_ms {
            dropped.push(step.clone());
        } else {
            placed.push(Scheduled {
                at_ms,
                step: step.clone(),
            });
        }
    }
    placed.sort_by_key(|s| s.at_ms);
    (placed, dropped)
}

/// A live summary entry, as the topic picker sees it.
#[derive(Debug, Clone)]
pub struct TopicEntry {
    pub kind: String,
    pub text: String,
}

/// The `{topic}` for a step: a short phrase from the latest decision, else
/// action, else point; a neutral phrase when the timeline is empty.
pub fn pick_topic(entries: &[TopicEntry]) -> String {
    let pick = ["decision", "action", "point"]
        .iter()
        .find_map(|kind| entries.iter().rev().find(|e| e.kind == *kind));
    let Some(entry) = pick else {
        return "the main topic so far".to_string();
    };
    let words: Vec<&str> = entry.text.split_whitespace().take(8).collect();
    let mut phrase = words.join(" ");
    while phrase.ends_with(['.', ',', ';', ':', '!', '?']) {
        phrase.pop();
    }
    let mut chars = phrase.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => "the main topic so far".to_string(),
    }
}

pub fn fill_topic(text: &str, topic: &str) -> String {
    text.replace("{topic}", topic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_script_parses() {
        let steps = parse(DEFAULT_SCRIPT).unwrap();
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[0].tag.as_deref(), Some("honesty"));
        assert_eq!(steps[0].at, At::Ms(300_000));
        assert_eq!(steps[1].kind, StepKind::Action);
        assert_eq!(steps[1].text, "what_to_say");
        assert_eq!(steps[3].at, At::PerMille(750));
    }

    #[test]
    fn bad_lines_are_reported_with_their_number() {
        let error = parse("05:00 ask hi\n10:00 dance now").unwrap_err();
        assert!(error.to_string().contains("line 2"), "{error}");
        assert!(parse("05:00 action sing").is_err());
        assert!(parse("5 ask hi").is_err());
        assert!(parse("120% ask hi").is_err());
        assert!(parse("05:00 ask").is_err());
    }

    #[test]
    fn schedule_orders_steps_and_drops_those_past_the_end() {
        let steps = parse("50:00 ask late\n50% ask half\n01:00 ask early").unwrap();
        let (placed, dropped) = schedule(&steps, 30 * 60_000);
        let times: Vec<u64> = placed.iter().map(|s| s.at_ms).collect();
        assert_eq!(times, vec![60_000, 900_000]);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].text, "late");
    }

    #[test]
    fn topic_prefers_the_latest_decision() {
        let entries = vec![
            TopicEntry {
                kind: "decision".into(),
                text: "Ship the beta on Friday.".into(),
            },
            TopicEntry {
                kind: "point".into(),
                text: "Pricing came up".into(),
            },
            TopicEntry {
                kind: "decision".into(),
                text: "Move the launch to the tenth because QA needs a full extra week".into(),
            },
        ];
        assert_eq!(
            pick_topic(&entries),
            "move the launch to the tenth because QA"
        );
        assert_eq!(
            pick_topic(&entries[1..2]),
            "pricing came up",
            "falls back to points"
        );
        assert_eq!(pick_topic(&[]), "the main topic so far");
        assert_eq!(
            fill_topic("What about {topic}?", "pricing"),
            "What about pricing?"
        );
    }
}
