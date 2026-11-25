-- Migration: a final pass can wait for the transcript models to download.
-- Right after the update to the FluidAudio speech models (or on a first
-- launch) Parakeet and the VAD take a while to download; a meeting that ends
-- meanwhile keeps its live transcript and records its pass here with
-- waiting_for_models = 1. The pass is queued once the models are on disk.
-- Startup's resume skips waiting rows and counts no attempt for them.

ALTER TABLE pending_final_passes ADD COLUMN waiting_for_models INTEGER NOT NULL DEFAULT 0;
