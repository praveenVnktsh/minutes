//! Live notes written after the fact, from a meeting's saved transcript.
//!
//! A meeting's Enhanced notes are only ever written from its live notes
//! (`summary::live_enhanced`). Where a meeting has none, or they stop short
//! (an import, a re-transcribed meeting, live notes off or failing, models
//! still downloading), the stretches they miss are written first: each
//! stretch of the saved transcript goes through the live worker's own
//! windows (`worker::run_offline`), with the same model choice, prompts and
//! fine-tuned adapter as live, and the notes land in the same timeline table.
//! Stretches the live notes cover are left alone.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use log::info;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use super::llm::Generator;
use super::scheduler::{Segment, Speaker};
use super::worker::{run_offline, OfflineStop, TimelineEntry, TimelineSnapshot, WorkerHost};
use crate::database::models::Transcript;
use crate::database::repositories::meeting::MeetingsRepository;
use crate::database::repositories::timeline::{TimelineEntryRow, TimelineRepository};
use crate::summary::live_outline::LiveNote;

/// A stretch of the meeting at least this long without a live note is
/// written after the fact. Live windows come every minute or so, so a gap
/// this long means the live notes were off or failing there.
pub const UNCOVERED_MIN_MS: i64 = 5 * 60_000;

/// The meeting's saved transcript as live segments, in audio order. Lines
/// without a recording-relative time cannot be placed and are left out.
pub fn segments_from_transcripts(rows: &[Transcript]) -> Vec<Segment> {
    let ms = |seconds: f64| (seconds.max(0.0) * 1000.0).round() as i64;
    let mut segments: Vec<Segment> = rows
        .iter()
        .filter(|row| !row.transcript.trim().is_empty())
        .filter_map(|row| {
            let start = row.audio_start_time.filter(|s| s.is_finite())?;
            let end = row
                .audio_end_time
                .filter(|e| e.is_finite())
                .or_else(|| row.duration.map(|d| start + d))
                .unwrap_or(start);
            let start_ms = ms(start);
            Some(Segment {
                start_ms,
                end_ms: ms(end).max(start_ms),
                speaker: Speaker::from_source(row.speaker.as_deref().unwrap_or_default()),
                text: row.transcript.trim().to_string(),
                received_at: std::time::Duration::ZERO,
            })
        })
        .collect();
    segments.sort_by_key(|s| (s.start_ms, s.end_ms));
    segments
}

/// The meeting's saved transcript as live segments.
pub async fn saved_segments(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<Segment>, String> {
    let (rows, _) =
        MeetingsRepository::get_meeting_transcripts_paginated(pool, meeting_id, i64::MAX, 0)
            .await
            .map_err(|e| format!("Could not load the transcript: {e}"))?;
    Ok(segments_from_transcripts(&rows))
}

/// The stretches of `segments` no live note covers, each at least
/// `UNCOVERED_MIN_MS` long: before the first note, between two notes, and
/// after the last one to the end of the transcript. Every segment that
/// starts inside one belongs to it. Notes made before live notes had topics
/// cannot carry the Enhanced notes, so a meeting with only those counts as
/// uncovered.
pub fn uncovered(notes: &[LiveNote], segments: &[Segment]) -> Vec<Vec<Segment>> {
    let has_topics = notes.iter().any(|n| n.topic.is_some());
    let mut covered: Vec<(i64, i64)> = notes
        .iter()
        .filter(|n| has_topics && !n.text.is_empty())
        .map(|n| (n.start_ms, n.end_ms.max(n.start_ms)))
        .collect();
    covered.sort();
    let end = segments.iter().map(|s| s.end_ms).max().unwrap_or(0);
    let mut gaps = Vec::new();
    let mut from = 0;
    for (start, stop) in covered {
        if start - from >= UNCOVERED_MIN_MS {
            gaps.push((from, start));
        }
        from = from.max(stop);
    }
    if end - from >= UNCOVERED_MIN_MS {
        gaps.push((from, i64::MAX));
    }
    gaps.into_iter()
        .map(|(start, stop)| {
            segments
                .iter()
                .filter(|s| s.start_ms >= start && s.start_ms < stop)
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|stretch| !stretch.is_empty())
        .collect()
}

fn entry_from_row(row: TimelineEntryRow) -> TimelineEntry {
    TimelineEntry {
        id: row.id,
        start_ms: row.start_ms,
        end_ms: row.end_ms,
        kind: row.kind,
        text: row.text,
        speaker: row.speaker,
        provider: row.provider,
        model: row.model,
        created_at: row.created_at,
        topic: row.topic,
        parent: None,
    }
}

/// Whether a recording is running now (its live notes come first).
fn recording_now() -> bool {
    use crate::meeting_activity::ActivityStatus;
    crate::meeting_activity::recording_identity().is_some_and(|r| {
        matches!(
            r.status,
            ActivityStatus::Starting | ActivityStatus::Recording | ActivityStatus::Paused
        )
    })
}

/// The worker's host after the meeting: one meeting, always on, its notes
/// stored in the timeline table, nothing shown live.
struct BackfillHost {
    pool: SqlitePool,
    meeting_id: String,
    generator: Arc<dyn Generator>,
}

#[async_trait]
impl WorkerHost for BackfillHost {
    fn is_paused(&self) -> bool {
        false
    }

    fn session_alive(&self) -> bool {
        true
    }

    fn meeting_id(&self) -> Option<String> {
        Some(self.meeting_id.clone())
    }

    fn enabled(&self) -> bool {
        true
    }

    async fn generator(&self) -> Result<Arc<dyn Generator>> {
        Ok(self.generator.clone())
    }

    async fn persist(&self, rows: &[TimelineEntryRow]) -> Result<()> {
        TimelineRepository::insert_entries(&self.pool, rows).await?;
        Ok(())
    }

    async fn meeting_exists(&self, meeting_id: &str) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    fn publish(&self, _: &TimelineSnapshot) {}

    async fn update_entries(&self, rows: &[TimelineEntryRow]) -> Result<()> {
        TimelineRepository::update_entries(&self.pool, rows).await?;
        Ok(())
    }

    async fn delete_entries(&self, meeting_id: &str, ids: &[String]) -> Result<()> {
        TimelineRepository::delete_entries(&self.pool, meeting_id, ids).await?;
        Ok(())
    }
}

/// Writes live notes for the stretches of the meeting's saved transcript that
/// `notes` (its live notes) do not cover, and stores them with the others.
/// Returns how many it wrote: 0 when the live notes cover the meeting.
pub async fn backfill(
    pool: &SqlitePool,
    meeting_id: &str,
    notes: &[LiveNote],
    generator: Arc<dyn Generator>,
    cancel: &CancellationToken,
) -> Result<usize, String> {
    let segments = saved_segments(pool, meeting_id).await?;
    let stretches = uncovered(notes, &segments);
    if stretches.is_empty() {
        return Ok(0);
    }
    let minutes: i64 = stretches
        .iter()
        .map(|s| {
            let start = s.first().map_or(0, |s| s.start_ms);
            let end = s.iter().map(|s| s.end_ms).max().unwrap_or(start);
            end - start
        })
        .sum::<i64>()
        / 60_000;
    info!(
        "Live notes backfill for {meeting_id}: {} stretches, about {minutes} min of transcript, with {}",
        stretches.len(),
        generator.model()
    );
    let existing = TimelineRepository::list_for_meeting(pool, meeting_id)
        .await
        .map_err(|e| format!("Could not load the live notes: {e}"))?
        .into_iter()
        .map(entry_from_row)
        .collect();
    let host = Arc::new(BackfillHost {
        pool: pool.clone(),
        meeting_id: meeting_id.to_string(),
        generator,
    });
    let written = run_offline(host, existing, stretches, &recording_now, cancel)
        .await
        .map_err(|stop| match stop {
            OfflineStop::Cancelled => stop.to_string(),
            OfflineStop::Failed(reason) => reason,
        })?;
    info!("Live notes backfill for {meeting_id}: {written} notes written");
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start_min: i64, topic: Option<&str>) -> LiveNote {
        LiveNote::new(
            start_min * 60_000,
            start_min * 60_000 + 30_000,
            "point",
            "a note",
            None,
            topic,
        )
    }

    /// One segment every 20 s for `minutes`.
    fn segments(minutes: i64) -> Vec<Segment> {
        (0..minutes * 3)
            .map(|i| Segment {
                start_ms: i * 20_000,
                end_ms: i * 20_000 + 15_000,
                speaker: Speaker::Others,
                text: format!("segment {i}"),
                received_at: std::time::Duration::ZERO,
            })
            .collect()
    }

    fn span(stretch: &[Segment]) -> (i64, i64) {
        (stretch[0].start_ms, stretch[stretch.len() - 1].start_ms)
    }

    #[test]
    fn a_meeting_without_live_notes_is_one_stretch() {
        let all = segments(12);
        let stretches = uncovered(&[], &all);
        assert_eq!(stretches.len(), 1);
        assert_eq!(stretches[0], all);
    }

    #[test]
    fn only_the_stretches_the_notes_miss_are_written() {
        // Notes every minute for the first 10 minutes, one at 25 minutes,
        // and a meeting of 40 minutes.
        let mut notes: Vec<LiveNote> = (0..10).map(|m| note(m, Some("Pricing"))).collect();
        notes.push(note(25, Some("Hiring")));
        let stretches = uncovered(&notes, &segments(40));
        let spans: Vec<(i64, i64)> = stretches.iter().map(|s| span(s)).collect();
        assert_eq!(
            spans,
            [
                // From the end of the 9-minute note to the 25-minute one.
                (9 * 60_000 + 40_000, 24 * 60_000 + 40_000),
                // After the last note to the end.
                (25 * 60_000 + 40_000, 39 * 60_000 + 40_000),
            ]
        );
    }

    #[test]
    fn short_gaps_and_a_covered_meeting_need_nothing() {
        let notes: Vec<LiveNote> = (0..10).map(|m| note(m * 2, Some("Pricing"))).collect();
        // A note every 2 minutes, the last at 18 minutes, a 21-minute meeting.
        assert!(uncovered(&notes, &segments(21)).is_empty());
    }

    #[test]
    fn notes_from_before_topics_cover_nothing() {
        let notes: Vec<LiveNote> = (0..10).map(|m| note(m, None)).collect();
        assert_eq!(uncovered(&notes, &segments(10)).len(), 1);
    }

    #[test]
    fn transcript_rows_become_segments_in_audio_order() {
        let row = |text: &str, start: Option<f64>, end: Option<f64>, speaker: &str| Transcript {
            id: text.into(),
            meeting_id: "m".into(),
            transcript: text.into(),
            timestamp: String::new(),
            speaker: Some(speaker.into()),
            summary: None,
            action_items: None,
            key_points: None,
            audio_start_time: start,
            audio_end_time: end,
            duration: None,
        };
        let segments = segments_from_transcripts(&[
            row("later", Some(12.5), Some(14.0), "system"),
            row("first", Some(1.0), None, "mic"),
            row("untimed", None, None, "mic"),
            row("  ", Some(3.0), Some(4.0), "mic"),
        ]);
        let got: Vec<(&str, i64, i64, Speaker)> = segments
            .iter()
            .map(|s| (s.text.as_str(), s.start_ms, s.end_ms, s.speaker))
            .collect();
        assert_eq!(
            got,
            [
                ("first", 1_000, 1_000, Speaker::You),
                ("later", 12_500, 14_000, Speaker::Others),
            ]
        );
    }
}
