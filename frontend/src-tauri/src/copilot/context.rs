//! What a Copilot request sees, and in what order. Pure.
//!
//! The user message is `stable` + `rest`. `stable` (live summary, then the
//! older part of the transcript window) changes at most every 30 seconds and
//! mostly by appending, so llama-helper can restore its saved state instead of
//! evaluating it again. Hard caps keep minute 60 as cheap as minute 5.

use super::prompt::{format_time, Task};

/// Who said a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    You,
    Others,
}

impl Speaker {
    /// `mic` is the user; everything else (system audio) is Others.
    pub fn from_source(source: &str) -> Self {
        if source == "mic" {
            Speaker::You
        } else {
            Speaker::Others
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Speaker::You => "You",
            Speaker::Others => "Others",
        }
    }
}

/// One finalized live transcript line. Times are audio time from the start
/// of the recording, the same clock the final transcript uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Speaker,
    pub text: String,
}

/// One live summary entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineItem {
    pub start_ms: i64,
    pub kind: String,
    pub text: String,
}

/// The transcript window: the last 3–5 minutes. Its start moves in 2-minute
/// steps, so the prompt prefix changes rarely.
pub const WINDOW_MS: i64 = 180_000;
const WINDOW_ALIGN_MS: i64 = 120_000;
/// Lines before this boundary belong to the stable prefix.
const STABLE_ALIGN_MS: i64 = 30_000;

/// Caps, in characters (about 4 per token).
pub const MAX_TIMELINE_CHARS: usize = 4_800;
pub const MAX_TRANSCRIPT_CHARS: usize = 6_400;
pub const MAX_NOTES_CHARS: usize = 1_600;
/// Character budgets for one prompt. The defaults are the hard caps; a
/// model with a small context gets a shrunken budget (see [`Budget::shrink`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub transcript_chars: usize,
    pub timeline_chars: usize,
    pub notes_chars: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            transcript_chars: MAX_TRANSCRIPT_CHARS,
            timeline_chars: MAX_TIMELINE_CHARS,
            notes_chars: MAX_NOTES_CHARS,
        }
    }
}

/// Floors a shrinking budget keeps, so a prompt always has some context.
const MIN_TRANSCRIPT_CHARS: usize = 1_200;
const MIN_TIMELINE_CHARS: usize = 600;
const MIN_NOTES_CHARS: usize = 300;

impl Budget {
    /// A smaller budget that cuts `excess_chars`: the oldest transcript goes
    /// first, then the live summary, then notes.
    pub fn shrink(self, excess_chars: usize) -> Self {
        let mut left = excess_chars;
        let mut take = |value: usize, floor: usize| {
            let cut = left.min(value.saturating_sub(floor));
            left -= cut;
            value - cut
        };
        let transcript_chars = take(self.transcript_chars, MIN_TRANSCRIPT_CHARS);
        let timeline_chars = take(self.timeline_chars, MIN_TIMELINE_CHARS);
        let notes_chars = take(self.notes_chars, MIN_NOTES_CHARS);
        Self {
            transcript_chars,
            timeline_chars,
            notes_chars,
        }
    }

    /// Whether it cannot shrink any further.
    pub fn at_floor(&self) -> bool {
        self.transcript_chars <= MIN_TRANSCRIPT_CHARS
            && self.timeline_chars <= MIN_TIMELINE_CHARS
            && self.notes_chars <= MIN_NOTES_CHARS
    }
}

/// Timeline entries are dropped in blocks, so the prefix moves rarely.
const TIMELINE_DROP_BLOCK: usize = 10;
/// Lines kept in memory: enough for the window plus a margin.
pub const KEEP_LINES_MS: i64 = 10 * 60_000;

/// A request's prompt, split for prefix reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptParts {
    pub system: String,
    /// The stable start of the user message.
    pub stable: String,
    /// Byte length of the most stable part of `stable` (the live summary),
    /// which survives the transcript window moving.
    pub anchor: usize,
    /// The rest of the user message: newest lines, notes, task.
    pub rest: String,
}

impl PromptParts {
    pub fn user(&self) -> String {
        format!("{}{}", self.stable, self.rest)
    }
}

fn line_text(line: &Line) -> String {
    format!(
        "[{}] {}: {}",
        format_time(line.start_ms),
        line.speaker.label(),
        line.text.trim()
    )
}

/// Inserts `line` in start order and forgets lines far older than any window.
pub fn push_line(lines: &mut Vec<Line>, line: Line) {
    let at = lines.partition_point(|l| l.start_ms <= line.start_ms);
    lines.insert(at, line);
    let newest = lines.iter().map(|l| l.end_ms).max().unwrap_or(0);
    let keep_from = newest - KEEP_LINES_MS;
    if lines.first().is_some_and(|l| l.end_ms < keep_from) {
        lines.retain(|l| l.end_ms >= keep_from);
    }
}

fn timeline_block(timeline: &[TimelineItem], max_chars: usize) -> String {
    if timeline.is_empty() {
        return "(nothing yet)\n".to_string();
    }
    let rendered: Vec<String> = timeline
        .iter()
        .map(|item| {
            let kind = match item.kind.as_str() {
                "decision" | "action" => format!("{}: ", item.kind),
                _ => String::new(),
            };
            format!(
                "[{}] {}{}\n",
                format_time(item.start_ms),
                kind,
                item.text.trim()
            )
        })
        .collect();
    let mut skip = 0;
    while skip < rendered.len()
        && rendered[skip..].iter().map(String::len).sum::<usize>() > max_chars
    {
        skip += TIMELINE_DROP_BLOCK;
    }
    let skip = skip.min(rendered.len());
    let mut block = String::new();
    if skip > 0 {
        block.push_str(&format!("(the first {skip} entries are left out)\n"));
    }
    for entry in &rendered[skip..] {
        block.push_str(entry);
    }
    block
}

fn clip_notes(notes: &str, max_chars: usize) -> String {
    let notes = notes.trim();
    match notes.char_indices().nth(max_chars) {
        // Keep the end: the newest notes are the most relevant.
        Some(_) => {
            let total = notes.chars().count();
            let tail: String = notes.chars().skip(total - max_chars).collect();
            format!("…{tail}")
        }
        None => notes.to_string(),
    }
}

/// Where the transcript window starts and where its stable part ends.
pub fn window_bounds(latest_ms: i64) -> (i64, i64) {
    let start = ((latest_ms - WINDOW_MS).max(0) / WINDOW_ALIGN_MS) * WINDOW_ALIGN_MS;
    let stable_cut = (latest_ms / STABLE_ALIGN_MS) * STABLE_ALIGN_MS;
    (start, stable_cut.max(start))
}

/// Builds the prompt for `task` from everything Copilot knows right now.
pub fn build(lines: &[Line], timeline: &[TimelineItem], notes: &str, task: &Task) -> PromptParts {
    build_with(lines, timeline, notes, task, Budget::default())
}

/// [`build`] within `budget`.
pub fn build_with(
    lines: &[Line],
    timeline: &[TimelineItem],
    notes: &str,
    task: &Task,
    budget: Budget,
) -> PromptParts {
    let latest = lines.iter().map(|l| l.end_ms).max().unwrap_or(0);
    let (window_start, stable_cut) = window_bounds(latest);
    let mut window: Vec<&Line> = lines.iter().filter(|l| l.end_ms > window_start).collect();
    // Over the cap (a very talkative window), drop the oldest lines.
    let mut total: usize = window.iter().map(|l| line_text(l).len() + 1).sum();
    while total > budget.transcript_chars && window.len() > 1 {
        total -= line_text(window[0]).len() + 1;
        window.remove(0);
    }

    let mut stable = String::new();
    stable.push_str("LIVE SUMMARY SO FAR:\n");
    stable.push_str(&timeline_block(timeline, budget.timeline_chars));
    let anchor = stable.len();
    stable.push_str("\nTRANSCRIPT, LAST FEW MINUTES:\n");
    let split = window.partition_point(|l| l.start_ms < stable_cut);
    for line in &window[..split] {
        stable.push_str(&line_text(line));
        stable.push('\n');
    }

    let mut rest = String::new();
    for line in &window[split..] {
        rest.push_str(&line_text(line));
        rest.push('\n');
    }
    if window.is_empty() {
        rest.push_str("(no speech yet)\n");
    }
    let notes = clip_notes(notes, budget.notes_chars);
    if !notes.is_empty() {
        rest.push_str("\nMY NOTES:\n");
        rest.push_str(&notes);
        rest.push('\n');
    }
    rest.push_str("\nTASK:\n");
    rest.push_str(&task.text());

    PromptParts {
        system: super::prompt::SYSTEM_PROMPT.to_string(),
        stable,
        anchor,
        rest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copilot::prompt::QuickAction;

    fn line(start_s: i64, speaker: Speaker, text: &str) -> Line {
        Line {
            start_ms: start_s * 1000,
            end_ms: start_s * 1000 + 4000,
            speaker,
            text: text.into(),
        }
    }

    fn meeting(minutes: i64) -> (Vec<Line>, Vec<TimelineItem>) {
        let mut lines = Vec::new();
        let mut t = 0;
        while t < minutes * 60 {
            let speaker = if t % 20 == 0 {
                Speaker::You
            } else {
                Speaker::Others
            };
            push_line(
                &mut lines,
                line(
                    t,
                    speaker,
                    "we talked about the onboarding funnel and the pricing tiers again",
                ),
            );
            t += 7;
        }
        let timeline = (0..minutes * 2)
            .map(|i| TimelineItem {
                start_ms: i * 30_000,
                kind: if i % 5 == 0 {
                    "decision".into()
                } else {
                    "point".into()
                },
                text: format!("Entry {i} about the roadmap and who owns the follow-up"),
            })
            .collect();
        (lines, timeline)
    }

    fn task() -> Task {
        Task::Action(QuickAction::WhatToSay)
    }

    #[test]
    fn layout_is_summary_then_older_lines_then_newest_lines_notes_and_task() {
        let lines = vec![
            line(5, Speaker::Others, "Can we ship Friday?"),
            line(40, Speaker::You, "I think so."),
        ];
        let timeline = vec![TimelineItem {
            start_ms: 0,
            kind: "decision".into(),
            text: "Ship on Friday".into(),
        }];
        let parts = build(&lines, &timeline, "check QA", &task());
        assert!(parts
            .stable
            .starts_with("LIVE SUMMARY SO FAR:\n[00:00] decision: Ship on Friday\n"));
        assert!(parts
            .stable
            .contains("[00:05] Others: Can we ship Friday?\n"));
        assert!(parts.rest.starts_with("[00:40] You: I think so.\n"));
        assert!(parts.rest.contains("MY NOTES:\ncheck QA"));
        assert!(parts.rest.ends_with(&task().text()));
        assert!(parts.system.contains("You: the person recording"));
        assert!(parts.stable[..parts.anchor].ends_with("Ship on Friday\n"));
    }

    #[test]
    fn only_the_last_few_minutes_of_transcript_are_included() {
        let (lines, timeline) = meeting(40);
        let parts = build(&lines, &timeline, "", &task());
        let user = parts.user();
        let transcript = &user[user.find("TRANSCRIPT").unwrap()..];
        assert!(
            !transcript.contains("[30:"),
            "half an hour ago is out of the window"
        );
        assert!(transcript.contains("[38:"), "the last minutes are in");
    }

    #[test]
    fn minute_sixty_costs_about_the_same_as_minute_five() {
        let (lines5, timeline5) = meeting(5);
        let (lines60, timeline60) = meeting(60);
        let notes = "n".repeat(10_000);
        let at5 = build(&lines5, &timeline5, &notes, &task()).user().len();
        let at60 = build(&lines60, &timeline60, &notes, &task()).user().len();
        let cap = MAX_TIMELINE_CHARS + MAX_TRANSCRIPT_CHARS + MAX_NOTES_CHARS + 2_000;
        assert!(at60 <= cap, "{at60} chars at minute 60");
        assert!(at60 < at5 * 3, "minute 60 ({at60}) vs minute 5 ({at5})");
    }

    #[test]
    fn the_prefix_is_stable_within_a_bucket_and_grows_by_appending() {
        let (mut lines, timeline) = meeting(20);
        let first = build(&lines, &timeline, "", &task());
        // A few seconds later in the same 30 s bucket.
        let last = lines.last().unwrap().start_ms / 1000;
        push_line(
            &mut lines,
            line(last + 1, Speaker::Others, "One more thing?"),
        );
        let second = build(&lines, &timeline, "", &task());
        // The stable part only ever grows at its end, so the saved prefix
        // still matches.
        assert!(second.stable.starts_with(&first.stable));
        // A different task never changes the prefix.
        let third = build(
            &lines,
            &timeline,
            "",
            &Task::Question("Who owns it?".into()),
        );
        assert_eq!(second.stable, third.stable);
        // Neither do notes.
        let fourth = build(&lines, &timeline, "new note", &task());
        assert_eq!(second.stable, fourth.stable);
    }

    #[test]
    fn a_long_timeline_drops_whole_blocks_from_the_start() {
        let timeline: Vec<TimelineItem> = (0..300)
            .map(|i| TimelineItem {
                start_ms: i * 10_000,
                kind: "point".into(),
                text: format!("entry {i} with enough words to matter for the budget"),
            })
            .collect();
        let block = timeline_block(&timeline, MAX_TIMELINE_CHARS);
        assert!(block.len() <= MAX_TIMELINE_CHARS + 60);
        let left_out: usize = block
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(left_out % 10, 0);
        assert!(block
            .trim_end()
            .ends_with("entry 299 with enough words to matter for the budget"));
    }

    #[test]
    fn shrinking_cuts_the_oldest_transcript_first_then_the_summary() {
        let budget = Budget::default().shrink(3_000);
        assert_eq!(budget.transcript_chars, MAX_TRANSCRIPT_CHARS - 3_000);
        assert_eq!(budget.timeline_chars, MAX_TIMELINE_CHARS);
        let budget = Budget::default().shrink(MAX_TRANSCRIPT_CHARS);
        assert_eq!(budget.transcript_chars, MIN_TRANSCRIPT_CHARS);
        assert!(budget.timeline_chars < MAX_TIMELINE_CHARS);
        assert!(Budget::default().shrink(usize::MAX).at_floor());

        let (lines, timeline) = meeting(60);
        let full = build(&lines, &timeline, "", &task());
        let small = build_with(
            &lines,
            &timeline,
            "",
            &task(),
            Budget::default().shrink(6_000),
        );
        assert!(small.user().len() < full.user().len() - 1_500);
        // The newest lines stay; the oldest go.
        let last = lines.last().unwrap();
        assert!(small
            .user()
            .contains(&format!("[{}]", format_time(last.start_ms))));
    }

    #[test]
    fn lines_are_kept_in_time_order_and_bounded() {
        let mut lines = Vec::new();
        push_line(&mut lines, line(10, Speaker::You, "b"));
        push_line(&mut lines, line(5, Speaker::Others, "a"));
        assert_eq!(lines[0].text, "a");
        push_line(&mut lines, line(2000, Speaker::You, "much later"));
        assert_eq!(lines.len(), 1, "lines far outside any window are dropped");
    }

    #[test]
    fn an_empty_meeting_says_so() {
        let parts = build(&[], &[], "", &task());
        assert!(parts.stable.contains("(nothing yet)"));
        assert!(parts.rest.contains("(no speech yet)"));
    }

    #[test]
    fn mic_is_you_and_system_is_others() {
        assert_eq!(Speaker::from_source("mic"), Speaker::You);
        assert_eq!(Speaker::from_source("system"), Speaker::Others);
    }
}
