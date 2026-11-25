-- Migration: remember queued final transcript passes across a quit or crash.
-- The transcription queue lives in memory, so a pass queued (or running) when
-- the app stopped was lost, and with it the meeting's speakers and automatic
-- summary. A row is written when the pass is queued and removed in the same
-- transaction that replaces the transcript (or when the pass gives up and the
-- live transcript is kept), so a row still present at startup means the pass
-- never finished and has to be queued again.

CREATE TABLE IF NOT EXISTS pending_final_passes (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    meeting_folder_path TEXT NOT NULL,
    language TEXT,
    force INTEGER NOT NULL DEFAULT 0,
    queued_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);
