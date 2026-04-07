CREATE TABLE IF NOT EXISTS favorites (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     TEXT NOT NULL,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, track_id)
);

CREATE TABLE IF NOT EXISTS listen_events (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         TEXT NOT NULL,
    track_id        UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    guild_id        TEXT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- NULL until the event is closed (track ends, skipped, or bot leaves vc)
    -- Set to elapsed playback time, not wall time.
    play_duration_ms BIGINT,
    -- Computed at event close: play_duration_ms / tracks.duration_ms >= 0.8
    -- (The 0.8 threshold is a named constant in the application layer.)
    completed       BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE IF NOT EXISTS playlists (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    owner_id    TEXT NOT NULL,
    -- 'private': only owner + collaborators can see
    -- 'public':  visible to all users who share a guild with the owner
    visibility  TEXT NOT NULL DEFAULT 'private'
                    CHECK (visibility IN ('private', 'public')),
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS playlist_items (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    playlist_id UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL DEFAULT 0,
    added_by    TEXT NOT NULL,   -- Discord user ID; owner or collaborator
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now()
    -- No UNIQUE on position — allows duplicate positions during reorder,
    -- resolved by added_at. Tracks are always ordered: position ASC, added_at ASC.
    -- Duplicate tracks in a playlist are intentionally allowed.
);

CREATE TABLE IF NOT EXISTS playlist_collaborators (
    playlist_id UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL,
    added_by    TEXT NOT NULL,   -- must be the playlist owner_id (enforced in app layer)
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (playlist_id, user_id)
);

-- ── Last.fm artist similarity cache ───────────────────────────
-- Populated once per artist at enrichment time by LastFmWorker.
-- Never populated at recommendation time — no live API calls in hot path.
-- source_mbid and similar_mbid are MusicBrainz Artist IDs.
-- An artist pair is stored even if similar_mbid is not yet in our artists table.
CREATE TABLE IF NOT EXISTS similar_artists (
    source_mbid      TEXT NOT NULL,
    similar_mbid     TEXT NOT NULL,
    similarity_score REAL NOT NULL CHECK (similarity_score >= 0 AND similarity_score <= 1),
    fetched_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_mbid, similar_mbid)
);

-- ── Materialized user-track affinities ────────────────────────
-- Top-K tracks recommended for each user, with decomposed signal scores.
-- Decomposed scores allow reweighting without recomputing raw signals.
-- Populated/updated eagerly on listen completion and favourite events.
-- track_id is a track the user has NOT yet heard (or rarely heard),
-- scored as a recommendation candidate.
CREATE TABLE IF NOT EXISTS user_track_affinities (
    user_id          TEXT NOT NULL,
    track_id         UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    favourites_score REAL NOT NULL DEFAULT 0,   -- similarity to user's favourites centroid
    acoustic_score   REAL NOT NULL DEFAULT 0,   -- similarity to user's taste centroid vector
    taste_score      REAL NOT NULL DEFAULT 0,   -- genre + artist affinity from listen history
    lastfm_score     REAL NOT NULL DEFAULT 0,   -- similar_artists graph proximity
    combined_score   REAL NOT NULL DEFAULT 0,   -- weighted blend (updated with weights)
    computed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, track_id)
);

-- ── Web portal: top genres per user per period ────────────────
-- Populated/updated on listen event close.
-- period_start / period_end define the time window (e.g., current calendar month).
CREATE TABLE IF NOT EXISTS user_genre_stats (
    user_id      TEXT NOT NULL,
    genre        TEXT NOT NULL,
    play_count   INTEGER NOT NULL DEFAULT 0,
    period_start TIMESTAMPTZ NOT NULL,
    period_end   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, genre, period_start)
);

-- ── Web portal: server-wide track popularity ──────────────────
-- Populated/updated on listen event close (completed = true only).
CREATE TABLE IF NOT EXISTS guild_track_stats (
    guild_id     TEXT NOT NULL,
    track_id     UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    play_count   INTEGER NOT NULL DEFAULT 0,
    period_start TIMESTAMPTZ NOT NULL,
    period_end   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (guild_id, track_id, period_start)
);
