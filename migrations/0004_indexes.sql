CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_fingerprint
    ON tracks(fingerprint_hash);

CREATE INDEX IF NOT EXISTS idx_tracks_blob_location
    ON tracks(blob_location);

-- idx_tracks_search_vector and idx_tracks_search_text removed in v3.
-- GIN indexes are no longer needed; Tantivy owns the search index.

CREATE INDEX IF NOT EXISTS idx_tracks_enrichment_queue
    ON tracks(enrichment_status, enrichment_attempts, enriched_at)
    WHERE enrichment_locked = false
      AND enrichment_status IN ('pending', 'failed', 'low_confidence', 'unmatched');

CREATE INDEX IF NOT EXISTS idx_favorites_user
    ON favorites(user_id);

CREATE INDEX IF NOT EXISTS idx_favorites_track
    ON favorites(track_id);

-- Efficient recent-history lookup per user
CREATE INDEX IF NOT EXISTS idx_listen_events_user_recent
    ON listen_events(user_id, started_at DESC);

-- Efficient per-track global play count for cold-start recommendations
CREATE INDEX IF NOT EXISTS idx_listen_events_track_global
    ON listen_events(track_id)
    WHERE completed = true;

CREATE INDEX IF NOT EXISTS idx_listen_events_track
    ON listen_events(track_id);
