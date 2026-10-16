-- Live Copilot moments live in the meeting's AI chat thread. kind is NULL
-- for ordinary chat, or live_ask / live_action / live_suggestion; label is
-- the quick action ("What should I say?"); audio_ms is the moment in the
-- recording it belongs to.
ALTER TABLE meeting_chat_messages ADD COLUMN kind TEXT;
ALTER TABLE meeting_chat_messages ADD COLUMN label TEXT;
ALTER TABLE meeting_chat_messages ADD COLUMN audio_ms INTEGER;
