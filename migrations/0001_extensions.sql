-- Extensions and custom functions required before any table creation.
-- This migration must run first and must be idempotent.

CREATE EXTENSION IF NOT EXISTS unaccent;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Standard unaccent() is STABLE, not IMMUTABLE.
-- Generated columns require IMMUTABLE functions.
-- This wrapper is the battle-tested solution.
CREATE OR REPLACE FUNCTION immutable_unaccent(text)
  RETURNS text LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
  $$ SELECT unaccent($1) $$;

-- Custom FTS config: no stemming, unaccent + lowercase only.
-- Stemming is deliberately excluded — it corrupts artist/album names.
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_ts_config WHERE cfgname = 'music_simple'
  ) THEN
    CREATE TEXT SEARCH CONFIGURATION music_simple (COPY = simple);
    ALTER TEXT SEARCH CONFIGURATION music_simple
      ALTER MAPPING FOR word, hword, hword_part
      WITH unaccent, simple;
  END IF;
END
$$;
