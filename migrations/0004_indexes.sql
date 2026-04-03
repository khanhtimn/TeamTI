CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_fingerprint
    ON tracks(audio_fingerprint)
    WHERE audio_fingerprint IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_tracks_blob_location
    ON tracks(blob_location);

CREATE INDEX IF NOT EXISTS idx_tracks_search_vector
    ON tracks USING GIN(search_vector);

CREATE INDEX IF NOT EXISTS idx_tracks_search_text
    ON tracks USING GIN(search_text gin_trgm_ops);

CREATE INDEX IF NOT EXISTS idx_tracks_enrichment_queue
    ON tracks(enrichment_status, enrichment_attempts, enriched_at)
    WHERE enrichment_locked = false
      AND enrichment_status IN ('pending', 'failed', 'low_confidence', 'unmatched');

CREATE INDEX IF NOT EXISTS idx_favorites_user
    ON favorites(user_id);

CREATE INDEX IF NOT EXISTS idx_listen_events_user
    ON listen_events(user_id, started_at DESC);

CREATE INDEX IF NOT EXISTS idx_listen_events_track
    ON listen_events(track_id);
