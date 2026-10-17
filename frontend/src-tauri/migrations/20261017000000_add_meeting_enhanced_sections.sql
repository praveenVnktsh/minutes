-- Enhanced notes written during the meeting: one row per live-notes topic
-- run, written when the topic closes (and at stop for the last one). They
-- are stored as they are written, so a crash mid-meeting keeps the finished
-- sections. Times are audio time from the recording start.
CREATE TABLE IF NOT EXISTS meeting_enhanced_sections (
    meeting_id TEXT NOT NULL,
    -- A topic run is identified by where it starts.
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    title TEXT NOT NULL,
    -- The section's bullets, Markdown, each ending in a [mm:ss] link.
    body TEXT NOT NULL,
    -- Open questions the section call reported, one per line.
    questions TEXT NOT NULL DEFAULT '',
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (meeting_id, start_ms),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);
