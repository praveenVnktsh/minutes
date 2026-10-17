//! Enhanced notes written during the meeting: when a live-notes topic closes
//! (the notetaker starts another one), the worker writes that topic's section
//! in the background, one section at a time, and stores it as soon as it is
//! written. The topic still under way is left for the stop
//! (`summary::live_enhanced::finish_notes`).
//!
//! This holds the worker's bookkeeping; the worker owns the task in flight.

use std::collections::{HashSet, VecDeque};

use serde::Serialize;

use crate::database::repositories::enhanced_sections::EnhancedSectionRow;
use crate::summary::live_outline::{
    live_view, topic_runs, LiveNote, LiveSection, TopicRun, TranscriptLine, WrittenSection,
};

pub const ENHANCED_UPDATE_EVENT: &str = "live-enhanced-update";
/// Tries per section during the meeting; after that it is written at stop.
pub const SECTION_ATTEMPTS: u32 = 2;

/// `live-enhanced-update` payload and `get_live_enhanced` result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnhancedSnapshot {
    pub session_id: String,
    pub meeting_id: Option<String>,
    pub sections: Vec<LiveSection>,
    /// "Also discussed", decisions, action items and open questions so far.
    pub tail: String,
}

/// A written section waiting for the meeting id (or a retried write).
#[derive(Debug, Clone)]
pub struct UnsavedSection {
    pub section: WrittenSection,
    pub end_ms: i64,
    pub provider: String,
    pub model: String,
}

impl UnsavedSection {
    pub fn row(&self, meeting_id: &str) -> EnhancedSectionRow {
        EnhancedSectionRow {
            meeting_id: meeting_id.to_string(),
            start_ms: self.section.start_ms,
            end_ms: self.end_ms,
            title: self.section.title.clone(),
            body: self.section.body.clone(),
            questions: self.section.questions.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// The sections of one session: written, queued, and which runs are taken.
#[derive(Debug, Default)]
pub struct Sections {
    pub written: Vec<WrittenSection>,
    pub queue: VecDeque<TopicRun>,
    /// Runs (by start) written, queued or in flight.
    claimed: HashSet<i64>,
    /// Failed tries per run.
    attempts: Vec<(i64, u32)>,
    pub unsaved: Vec<UnsavedSection>,
}

impl Sections {
    /// Queues every closed run (all but the last) that needs the model and
    /// is not written, queued or in flight. Returns how many were queued.
    pub fn queue_closed(&mut self, runs: &[TopicRun]) -> usize {
        let closed = runs.len().saturating_sub(1);
        let mut queued = 0;
        for run in &runs[..closed] {
            if run.is_small() || self.claimed.contains(&run.start_ms) {
                continue;
            }
            if self.tries(run.start_ms) >= SECTION_ATTEMPTS {
                continue;
            }
            self.claimed.insert(run.start_ms);
            self.queue.push_back(run.clone());
            queued += 1;
        }
        queued
    }

    fn tries(&self, start_ms: i64) -> u32 {
        self.attempts
            .iter()
            .find(|(start, _)| *start == start_ms)
            .map_or(0, |(_, n)| *n)
    }

    pub fn written(&mut self, section: UnsavedSection) {
        self.written
            .retain(|w| w.start_ms != section.section.start_ms);
        self.written.push(section.section.clone());
        self.written.sort_by_key(|w| w.start_ms);
        self.unsaved.push(section);
    }

    /// A try failed: the run may be queued again by the next window, until
    /// it is out of tries (then the stop writes it).
    pub fn failed(&mut self, start_ms: i64) {
        self.claimed.remove(&start_ms);
        match self
            .attempts
            .iter_mut()
            .find(|(start, _)| *start == start_ms)
        {
            Some((_, n)) => *n += 1,
            None => self.attempts.push((start_ms, 1)),
        }
    }

    pub fn view(
        &self,
        session_id: &str,
        meeting_id: Option<String>,
        notes: &[LiveNote],
        transcript: &[TranscriptLine],
        end_ms: i64,
    ) -> EnhancedSnapshot {
        let runs = topic_runs(notes, end_ms);
        let (sections, tail) = live_view(&runs, &self.written, notes, transcript);
        EnhancedSnapshot {
            session_id: session_id.to_string(),
            meeting_id,
            sections,
            tail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(start_ms: i64, notes: usize) -> TopicRun {
        TopicRun {
            title: format!("Topic {start_ms}"),
            start_ms,
            end_ms: start_ms + 60_000,
            notes: (0..notes)
                .map(|i| {
                    LiveNote::new(
                        start_ms + i as i64,
                        start_ms + i as i64,
                        "point",
                        "a note",
                        None,
                        Some("x"),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn closed_runs_are_queued_once_small_and_open_ones_never() {
        let mut sections = Sections::default();
        let runs = vec![run(0, 3), run(60_000, 1), run(120_000, 4), run(180_000, 5)];
        assert_eq!(sections.queue_closed(&runs), 2);
        assert_eq!(sections.queue_closed(&runs), 0);
        let starts: Vec<i64> = sections.queue.iter().map(|r| r.start_ms).collect();
        assert_eq!(starts, [0, 120_000]);
    }

    #[test]
    fn a_failed_run_is_retried_until_it_is_out_of_tries() {
        let mut sections = Sections::default();
        let runs = vec![run(0, 3), run(60_000, 3)];
        sections.queue_closed(&runs);
        sections.queue.clear();
        sections.failed(0);
        assert_eq!(sections.queue_closed(&runs), 1);
        sections.queue.clear();
        sections.failed(0);
        assert_eq!(sections.queue_closed(&runs), 0, "left for the stop");
    }
}
