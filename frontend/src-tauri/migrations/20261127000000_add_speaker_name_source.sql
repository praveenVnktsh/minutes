-- Who named a speaker: 'user' (a rename or merge) or 'auto' (names seeded by
-- diarization, such as "You" or the meeting's one other attendee). A
-- diarization rerun may replace or clear 'auto' names and never touches
-- 'user' ones.
ALTER TABLE speaker_identities ADD COLUMN name_source TEXT NOT NULL DEFAULT 'user';
