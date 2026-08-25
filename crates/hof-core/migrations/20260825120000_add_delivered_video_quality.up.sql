-- Record the video stream that was actually delivered, not just the one requested.
--
-- A profile's `quality` is a preference negotiated against what the platform
-- publishes, so it is not evidence of what landed on disk. Persisting the
-- delivered height and codec makes under-delivery visible and lets the UI label
-- a download with its real resolution.
ALTER TABLE videos
ADD COLUMN video_height INTEGER,
ADD COLUMN video_codec TEXT;
