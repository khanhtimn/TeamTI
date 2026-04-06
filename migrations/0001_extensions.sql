-- Extensions and custom functions required before any table creation.
-- This migration must run first and must be idempotent.
--
-- NOTE (v3): pg_trgm and the music_simple FTS configuration have been
-- removed. Full-text search is now handled by the embedded Tantivy index
-- (adapters-search). The unaccent extension is retained because
-- immutable_unaccent() is used by the Tantivy rebuild query.

CREATE EXTENSION IF NOT EXISTS unaccent;

-- Standard unaccent() is STABLE, not IMMUTABLE.
-- Generated columns require IMMUTABLE functions.
CREATE OR REPLACE FUNCTION immutable_unaccent(text)
RETURNS text LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT unaccent($1) $$;
