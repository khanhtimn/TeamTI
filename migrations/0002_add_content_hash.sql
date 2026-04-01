ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS content_hash TEXT;
ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS original_filename TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_media_assets_content_hash
    ON media_assets(content_hash) WHERE content_hash IS NOT NULL;

ALTER TABLE media_assets ADD COLUMN IF NOT EXISTS search_vector tsvector
    GENERATED ALWAYS AS (
        to_tsvector('english', coalesce(title, '') || ' ' || coalesce(original_filename, ''))
    ) STORED;

CREATE INDEX IF NOT EXISTS idx_media_assets_search ON media_assets USING GIN(search_vector);
