-- Live summary timeline: short entries made during a recording, shown as the
-- meeting's chapters afterwards. Times are audio time from the recording start.
CREATE TABLE IF NOT EXISTS meeting_timeline_entries (
    id TEXT PRIMARY KEY NOT NULL,
    meeting_id TEXT NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('point', 'decision', 'action')),
    text TEXT NOT NULL,
    speaker TEXT,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_meeting_timeline_entries_meeting
    ON meeting_timeline_entries(meeting_id, start_ms);
