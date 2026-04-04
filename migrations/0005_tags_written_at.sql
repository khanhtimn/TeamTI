-- Pass 4: Tag writeback tracking column.
-- tags_written_at records when enriched metadata was written back to the audio file.
-- NULL = not yet written (either new track or re-enriched since last write).

ALTER TABLE tracks ADD COLUMN IF NOT EXISTS
    tags_written_at TIMESTAMPTZ DEFAULT NULL;

-- Index on (updated_at ASC) for the startup poller query that
-- orders by updated_at. A partial index on (id) would not help with the
-- ORDER BY updated_at clause, forcing a sequential scan + sort.
CREATE INDEX IF NOT EXISTS idx_tracks_tags_unwritten
    ON tracks (updated_at ASC)
    WHERE enrichment_status = 'done' AND tags_written_at IS NULL;
