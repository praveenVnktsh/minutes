-- Migration: Linear and Notion connections for the people directory.
-- One row per connected source. The token sits in this local database next to
-- the LLM provider API keys (settings.*ApiKey) and is only ever sent to that
-- source's own API.

CREATE TABLE IF NOT EXISTS people_integrations (
    source TEXT PRIMARY KEY,         -- linear | notion
    token TEXT NOT NULL,
    -- JSON: for Notion, {"database_id", "database_title", "mapping"}.
    config TEXT NOT NULL DEFAULT '{}',
    workspace TEXT,
    last_synced_at TEXT,             -- last successful sync (RFC 3339)
    last_attempt_at TEXT,
    last_error TEXT,                 -- NULL after a successful sync
    last_count INTEGER,              -- people synced last time
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
