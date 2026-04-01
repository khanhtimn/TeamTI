CREATE TABLE media_assets (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    origin_type VARCHAR(50) NOT NULL,
    origin_rel_path VARCHAR(1024),
    origin_remote_url VARCHAR(2048),
    duration_ms BIGINT,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW() NOT NULL
);

CREATE TABLE playlists (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    owner_id BIGINT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW() NOT NULL
);

CREATE TABLE playlist_items (
    id UUID,
    playlist_id UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    asset_id UUID NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (id)
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
