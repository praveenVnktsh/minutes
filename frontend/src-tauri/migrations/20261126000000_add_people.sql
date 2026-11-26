-- Migration: a local directory of people and who attended each meeting.
-- People come from Linear, Notion, calendar invites or are typed in by the
-- user. Nothing here is sent anywhere: it is read to help transcription
-- (names as vocabulary), speaker naming, action item owners and Copilot.

CREATE TABLE IF NOT EXISTS people (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    -- Lowercased; the merge key across sources when present.
    email TEXT,
    role TEXT,
    team TEXT,
    -- JSON array of strings: nicknames and short names ("Dev").
    aliases TEXT NOT NULL DEFAULT '[]',
    -- JSON array of {"title","url","source"}: active projects and issues.
    projects TEXT NOT NULL DEFAULT '[]',
    notes TEXT,
    -- 1 for the person using this Mac.
    is_me INTEGER NOT NULL DEFAULT 0,
    -- JSON array of field names the user edited by hand; syncs leave them.
    locked_fields TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_people_email ON people(email) WHERE email IS NOT NULL;

-- Where each person came from, so a re-sync updates the same row.
CREATE TABLE IF NOT EXISTS person_sources (
    source TEXT NOT NULL,        -- linear | notion | calendar | manual
    external_id TEXT NOT NULL,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    synced_at TEXT NOT NULL,
    PRIMARY KEY (source, external_id)
);

CREATE INDEX IF NOT EXISTS idx_person_sources_person ON person_sources(person_id);

CREATE TABLE IF NOT EXISTS meeting_attendees (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    source TEXT NOT NULL,        -- calendar | manual
    created_at TEXT NOT NULL,
    PRIMARY KEY (meeting_id, person_id)
);

CREATE INDEX IF NOT EXISTS idx_meeting_attendees_person ON meeting_attendees(person_id);
