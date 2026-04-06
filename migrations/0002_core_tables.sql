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
    release_date    DATE,
    total_tracks    INTEGER,
    total_discs     INTEGER DEFAULT 1,
    mbid            TEXT UNIQUE,
    record_label    TEXT,
    upc_barcode     TEXT,
    genres          TEXT[],
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
    genres          TEXT[],
    year            INTEGER,
    bpm             INTEGER,
    isrc            TEXT,
    lyrics          TEXT,
    bitrate         INTEGER,
    sample_rate     INTEGER,
    channels        INTEGER,
    codec           TEXT,

    audio_fingerprint   TEXT,
    -- md5 hash of audio_fingerprint for indexing. Chromaprint encoded
    -- fingerprints are ~3500 bytes, exceeding btree's 2704-byte row limit.
    -- The 32-char hex hash is used for the UNIQUE index and ON CONFLICT.
    fingerprint_hash    TEXT GENERATED ALWAYS AS (md5(audio_fingerprint)) STORED,
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
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()


    -- ── Audio analysis (bliss-audio) ──────────────────────────────────
    -- Mirrors the enrichment_status pattern for traceability.
    -- analysis_status = 'pending'    → not yet analysed
    --                  'processing'  → currently locked by analysis worker
    --                  'done'        → bliss_vector is populated
    --                  'failed'      → analysis attempt failed (file unreadable, decode error)
    -- Triggers on all tracks regardless of enrichment_status.
    , analysis_status   TEXT NOT NULL DEFAULT 'pending'
                          CHECK (analysis_status IN ('pending', 'processing', 'done', 'failed'))
    , analysis_attempts INTEGER NOT NULL DEFAULT 0
    , analysis_locked   BOOLEAN NOT NULL DEFAULT false
    , analyzed_at       TIMESTAMPTZ

    -- 23-dimensional bliss-audio feature vector (Euclidean distance space).
    -- Dimension verified at compile time via bliss_audio::NUMBER_FEATURES.
    -- Version2 (LATEST) uses 23 features.
    -- NULL until analysis_status = 'done'.
    -- Query with: ORDER BY bliss_vector <-> $seed_vector (L2 distance)
    , bliss_vector      vector(23)
);

CREATE TABLE IF NOT EXISTS track_artists (
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    artist_id   UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'primary',
    position    INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (track_id, artist_id, role)
);
