-- Live notes: the topic (a short section title) each timeline entry sits
-- under. NULL for entries made before topics existed; readers show those
-- without a section header.
ALTER TABLE meeting_timeline_entries ADD COLUMN topic TEXT;
