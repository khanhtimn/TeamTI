-- 0002: Multilingual full-text search and trigram fuzzy matching
-- Extensions, text search configuration, generated columns, and GIN indexes.
-- NFC normalization is applied at the DB level as belt-and-suspenders
-- (the Rust application layer also normalizes before insertion).
-- Requires server_encoding = UTF8 for normalize().

-- Extensions (bundled with PostgreSQL, no external install needed)
CREATE EXTENSION IF NOT EXISTS unaccent;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- IMMUTABLE wrapper around unaccent() so it can be used in generated columns
-- and functional indexes. The built-in unaccent() is STABLE (not IMMUTABLE)
-- because it reads dictionary files, but in practice its output never changes
-- for the same input.
CREATE OR REPLACE FUNCTION immutable_unaccent(text)
RETURNS text AS $$
    SELECT public.unaccent('public.unaccent', $1)
$$ LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT;

-- Custom text search config: simple tokenization + unaccent normalization.
-- Language-agnostic — no stemming, which is correct for music metadata
-- (artist names and song titles should not be stemmed).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_ts_dict WHERE dictname = 'unaccent_simple'
    ) THEN
        CREATE TEXT SEARCH DICTIONARY unaccent_simple (
            TEMPLATE = unaccent,
            RULES    = 'unaccent'
        );
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_ts_config WHERE cfgname = 'music_simple'
    ) THEN
        CREATE TEXT SEARCH CONFIGURATION music_simple (COPY = simple);

        ALTER TEXT SEARCH CONFIGURATION music_simple
            ALTER MAPPING FOR hword, hword_part, word
            WITH unaccent_simple, simple;
    END IF;
END
$$;

-- Normalized plaintext column for trigram fuzzy matching.
-- Pipeline: NFC normalize → unaccent → lowercase
ALTER TABLE media_assets ADD COLUMN search_text TEXT
    GENERATED ALWAYS AS (
        lower(
            immutable_unaccent(
                normalize(coalesce(title, ''), NFC)
                || ' ' || normalize(coalesce(artist, ''), NFC)
                || ' ' || normalize(coalesce(original_filename, ''), NFC)
            )
        )
    ) STORED;

-- FTS tsvector using music_simple (language-agnostic, diacritics normalized)
ALTER TABLE media_assets ADD COLUMN search_vector tsvector
    GENERATED ALWAYS AS (
        to_tsvector('music_simple',
            normalize(coalesce(title, ''), NFC)
            || ' ' || normalize(coalesce(artist, ''), NFC)
            || ' ' || normalize(coalesce(original_filename, ''), NFC)
        )
    ) STORED;

-- GIN index for tsvector FTS queries (prefix and boolean matching)
CREATE INDEX idx_media_assets_search
    ON media_assets USING GIN(search_vector);

-- GIN trigram index for fuzzy, substring, and partial matching
CREATE INDEX idx_media_assets_trgm
    ON media_assets USING GIN(search_text gin_trgm_ops);
