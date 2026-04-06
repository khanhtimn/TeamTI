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
    play_duration_ms INTEGER,
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
