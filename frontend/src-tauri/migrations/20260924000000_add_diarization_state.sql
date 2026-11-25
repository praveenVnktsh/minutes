-- Migration: track whether a meeting's current transcript has speaker labels.
-- `diarization_runs` keeps every run, but a retranscription replaces the
-- transcript rows without clearing old runs, so it cannot say whether the rows
-- on screen were diarized. One row per meeting records that instead:
--   pending  diarization is owed (queued, running, or interrupted by a quit)
--   done     the current transcript rows carry speaker labels
--   failed   the last run errored; `error` says why
-- `attempts` counts runs started since the last success, so the startup
-- resume gives up on a meeting that keeps failing or crashing.

CREATE TABLE IF NOT EXISTS diarization_state (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_diarization_state_status
    ON diarization_state(status, updated_at);
