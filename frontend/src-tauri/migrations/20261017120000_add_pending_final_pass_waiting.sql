-- Migration: a final pass can wait for the Mac to be plugged in.
-- With "Improve transcript after meetings: When plugged in", a meeting that
-- ends on battery keeps its live transcript and records its pass here with
-- waiting_for_power = 1; the pass is queued once the Mac is on power (or the
-- user asks for it). Startup's resume skips waiting rows.

ALTER TABLE pending_final_passes ADD COLUMN waiting_for_power INTEGER NOT NULL DEFAULT 0;
