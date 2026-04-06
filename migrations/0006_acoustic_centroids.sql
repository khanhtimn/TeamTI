-- Add user_acoustic_centroids table to cache centroid calculation 
CREATE TABLE IF NOT EXISTS user_acoustic_centroids (
    user_id     TEXT PRIMARY KEY,
    centroid    vector(23),
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
