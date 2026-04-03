CREATE TABLE IF NOT EXISTS artists (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    sort_name   TEXT NOT NULL,
    mbid        TEXT UNIQUE,
    country     TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS albums (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title           TEXT NOT NULL,
    release_year    INTEGER,
    total_tracks    INTEGER,
    total_discs     INTEGER DEFAULT 1,
    mbid            TEXT UNIQUE,
    cover_art_path  TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS album_artists (
    album_id    UUID NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    artist_id   UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'primary',
    position    INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (album_id, artist_id)
);

CREATE TABLE IF NOT EXISTS tracks (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    title           TEXT NOT NULL,
    artist_display  TEXT,
    album_id        UUID REFERENCES albums(id),
    track_number    INTEGER,
    disc_number     INTEGER DEFAULT 1,
    duration_ms     INTEGER,
    genre           TEXT,
    year            INTEGER,

    audio_fingerprint   TEXT,
    file_modified_at    TIMESTAMPTZ,
    file_size_bytes     BIGINT,
    blob_location       TEXT NOT NULL,

    mbid                    TEXT,
    acoustid_id             TEXT,
    enrichment_status       TEXT NOT NULL DEFAULT 'pending',
    enrichment_confidence   REAL,
    enrichment_attempts     INTEGER NOT NULL DEFAULT 0,
    enrichment_locked       BOOLEAN NOT NULL DEFAULT false,
    enriched_at             TIMESTAMPTZ,

    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),

    search_text TEXT GENERATED ALWAYS AS (
        lower(immutable_unaccent(
            normalize(coalesce(title, ''), NFC) || ' ' ||
            normalize(coalesce(artist_display, ''), NFC) || ' ' ||
            normalize(coalesce(genre, ''), NFC)
        ))
    ) STORED,

    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('music_simple',
            normalize(coalesce(title, ''), NFC) || ' ' ||
            normalize(coalesce(artist_display, ''), NFC)
        )
    ) STORED
);

CREATE TABLE IF NOT EXISTS track_artists (
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    artist_id   UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'primary',
    position    INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (track_id, artist_id, role)
);
