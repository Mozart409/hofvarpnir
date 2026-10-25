-- Quality gate: a new video whose best format is still below the profile's
-- quality is held in `awaiting_quality` and rechecked (via `next_retry`) until
-- the platform finishes processing higher resolutions or the deadline passes.
ALTER TYPE video_status ADD VALUE IF NOT EXISTS 'awaiting_quality' AFTER 'pending';

-- Hours after publication a video may wait for its profile's quality.
-- NULL falls back to QUALITY_WAIT_HOURS (default 24); 0 disables the gate.
ALTER TABLE profiles
ADD COLUMN quality_wait_hours INTEGER CHECK (quality_wait_hours >= 0);

-- Set by Download now; the worker skips the quality gate for this video.
ALTER TABLE videos
ADD COLUMN skip_quality_gate BOOLEAN NOT NULL DEFAULT false;
