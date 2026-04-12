-- ── YouTube metadata columns on tracks ───────────────────────────────────
-- All nullable — NULL for local tracks, populated for YouTube-sourced tracks.
ALTER TABLE tracks
    ADD COLUMN IF NOT EXISTS source             TEXT NOT NULL DEFAULT 'local'
        CHECK (source IN ('local', 'youtube')),
    ADD COLUMN IF NOT EXISTS youtube_video_id   TEXT,
    ADD COLUMN IF NOT EXISTS youtube_channel_id TEXT,
    ADD COLUMN IF NOT EXISTS youtube_uploader   TEXT,
    ADD COLUMN IF NOT EXISTS youtube_thumbnail_url TEXT;

-- Make blob_location nullable for YouTube stubs (no file downloaded yet).
ALTER TABLE tracks ALTER COLUMN blob_location DROP NOT NULL;

-- Unique index: prevents duplicate stubs for the same video.
-- Partial index: only on rows where youtube_video_id IS NOT NULL.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_youtube_video_id
    ON tracks(youtube_video_id)
    WHERE youtube_video_id IS NOT NULL;

-- ── YouTube download job queue ────────────────────────────────────────────
-- One row per unique video_id. Tracks the lifecycle of the background download.
-- status:
--   'pending'           → waiting for a download slot
--   'downloading'       → yt-dlp subprocess is running
--   'done'              → file written, blob_location set on tracks row
--   'failed'            → this attempt failed; will retry up to max_attempts
--   'permanently_failed'→ max attempts exhausted; stub may be cleaned up
CREATE TABLE IF NOT EXISTS youtube_download_jobs (
    video_id              TEXT PRIMARY KEY,
    track_id              UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    url                   TEXT NOT NULL,
    status                TEXT NOT NULL DEFAULT 'pending'
                              CHECK (status IN (
                                  'pending','downloading','done',
                                  'failed','permanently_failed'
                              )),
    attempts              INTEGER NOT NULL DEFAULT 0,
    error_message         TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at            TIMESTAMPTZ,
    completed_at          TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_youtube_download_jobs_status
    ON youtube_download_jobs(status, attempts)
    WHERE status IN ('pending', 'failed');
