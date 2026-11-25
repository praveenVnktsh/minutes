-- Migration: remember when the user has chosen a meeting's title.
-- Meetings are named automatically: "Meeting DD_MM_YY_HH_MM_SS" at the start,
-- then a generated title once the AI notes are written. Once the user renames
-- a meeting (or asks for a new title with "Regenerate title"), title_edited is
-- set to 1 and no automatic path may change the title again: the notes at
-- stop, Regenerate AI notes, and the save at the end of a recording or a
-- resumed recording all keep it.
-- Existing meetings cannot be told apart reliably, so they start unedited.

ALTER TABLE meetings ADD COLUMN title_edited INTEGER NOT NULL DEFAULT 0;
