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

-- Analysis worker queue (mirrors idx_tracks_enrichment_queue)
CREATE INDEX IF NOT EXISTS idx_tracks_analysis_queue
ON tracks(analysis_status, analysis_attempts, analyzed_at)
WHERE analysis_locked = false
  AND analysis_status IN ('pending', 'failed');

-- Vector similarity: used by pgvector ANN queries
-- For 20-dim vectors at 50k rows, HNSW is optional — add if query
-- latency exceeds 5ms under load.
CREATE INDEX IF NOT EXISTS idx_tracks_bliss_vector
ON tracks USING hnsw (bliss_vector vector_l2_ops);

-- Last.fm lookup
CREATE INDEX IF NOT EXISTS idx_similar_artists_source
ON similar_artists(source_mbid);

-- Affinity recommendations for a user, ranked by combined score
CREATE INDEX IF NOT EXISTS idx_user_track_affinities_user
ON user_track_affinities(user_id, combined_score DESC);

-- Discovery page: genre trends
CREATE INDEX IF NOT EXISTS idx_user_genre_stats_user
ON user_genre_stats(user_id, period_start DESC, play_count DESC);

-- Discovery page: server popularity
CREATE INDEX IF NOT EXISTS idx_guild_track_stats_guild
ON guild_track_stats(guild_id, period_start DESC, play_count DESC);
