-- Migration: count how many times startup has resumed each pending final pass.
-- A pass that kills the app (a crash in the model, say) would otherwise be
-- resumed on every launch for the whole resume window; after a few attempts
-- the resume gives up and keeps the live transcript instead.

ALTER TABLE pending_final_passes ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
