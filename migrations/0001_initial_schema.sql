CREATE TABLE media_assets (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    artist TEXT,
    origin_type VARCHAR(50) NOT NULL,
    origin_rel_path VARCHAR(1024),
    origin_remote_url VARCHAR(2048),
    duration_ms BIGINT,
    content_hash TEXT,
    original_filename TEXT,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW() NOT NULL
);

CREATE UNIQUE INDEX idx_media_assets_content_hash
    ON media_assets(content_hash) WHERE content_hash IS NOT NULL;

CREATE TABLE playlists (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    owner_id BIGINT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW() NOT NULL
);

CREATE TABLE playlist_items (
    id UUID PRIMARY KEY,
    playlist_id UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    asset_id UUID NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
    position INTEGER NOT NULL
);

CREATE TABLE guild_settings (
    guild_id BIGINT PRIMARY KEY,
    config JSONB DEFAULT '{}'::jsonb NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW() NOT NULL
);

CREATE TABLE queue_history (
    id UUID PRIMARY KEY,
    guild_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    asset_id UUID NOT NULL REFERENCES media_assets(id) ON DELETE SET NULL,
    played_at TIMESTAMP WITH TIME ZONE DEFAULT NOW() NOT NULL
);

CREATE TABLE provider_links (
    id UUID PRIMARY KEY,
    provider VARCHAR(50) NOT NULL,
    remote_id VARCHAR(255) NOT NULL,
    asset_id UUID NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
    UNIQUE(provider, remote_id)
);
