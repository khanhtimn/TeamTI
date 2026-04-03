# TeamTI v2 — Pass 1 Implementation Prompt
## Foundation: Domain, Ports, Config, Migrations

---

### Context

You are implementing Pass 1 of the TeamTI v2 rewrite. TeamTI is a self-hosted
Rust Discord music bot. The canonical design reference is the TeamTI v2 Master
Requirements & Design Document. Read it fully before writing any code.

Pass 1 establishes the **foundational layer** that all subsequent passes depend
on. Its single goal is: the project compiles cleanly against new v2 types, and
the database migrations run successfully on a fresh Postgres instance.

No pipeline logic, no HTTP clients, no file watching, and no Discord command
changes are part of this pass.

---

### Acceptance Criteria

Pass 1 is complete when ALL of the following are true:

- [ ] `cargo build --workspace` produces zero errors and zero warnings
- [ ] `cargo test --workspace` passes (adapt any v1 tests that reference
      removed types; do not delete tests, port them to new types)
- [ ] `sqlx migrate run` against a fresh Postgres database succeeds with
      all migrations applied cleanly
- [ ] `sqlx migrate run` is idempotent (running it twice produces no error)
- [ ] All domain structs derive `Debug`, `Clone`, `serde::Serialize`,
      `serde::Deserialize`, and `sqlx::FromRow` where applicable
- [ ] All port traits are defined with correct async signatures using
      `async_trait::async_trait`
- [ ] The `Config` struct loads all v2 fields from environment variables
      without panicking on missing optional fields (defaults applied)
- [ ] No references to `media_assets`, `MediaAsset`, or any v1-only type
      remain anywhere in the codebase

---

### Scope: What This Pass Touches

| Crate | Action |
|---|---|
| `crates/domain/` | **Rewrite** — replace v1 entities with v2 entities |
| `crates/application/` | **Rewrite** — replace v1 ports with v2 ports |
| `crates/shared-config/` | **Extend** — add all v2 config fields |
| `crates/adapters-persistence/` | **Extend** — add migrations 0001–000x, stub repositories |
| `Cargo.toml` (workspace) | **Extend** — add new dependency versions |

### Scope: What This Pass Does NOT Touch

- `crates/adapters-discord/` — leave all v1 commands compiling as-is;
  update only import paths if domain types were renamed
- `crates/adapters-voice/` — leave unchanged; update import paths only
- `crates/adapters-media-store/` — leave unchanged; update import paths only
- `apps/bot/` — update wiring only enough to compile; do not add new adapters
- No new crates are created in this pass

---

### Step 1 — Workspace `Cargo.toml` additions

Add the following to `[workspace.dependencies]` if not already present.
Do not change existing versions.

```toml
# Domain
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }

# Persistence
sqlx = { version = "0.8", features = [
    "runtime-tokio-native-tls",
    "postgres",
    "uuid",
    "chrono",
    "migrate",
] }

# Config
dotenvy = "0.15"
```

---

### Step 2 — `crates/domain/`

Delete all existing files in `src/`. Replace with the following module
structure. Every struct and enum must compile with zero warnings.

#### `src/lib.rs`

```rust
pub mod artist;
pub mod album;
pub mod track;
pub mod user_library;
pub mod enrichment;

pub use artist::{Artist, ArtistRole, AlbumArtist, TrackArtist};
pub use album::Album;
pub use track::Track;
pub use user_library::{Favorite, ListenEvent, Playlist, PlaylistItem};
pub use enrichment::EnrichmentStatus;
```

#### `src/enrichment.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum EnrichmentStatus {
    Pending,
    Enriching,
    Done,
    LowConfidence,
    Unmatched,
    Failed,
    Exhausted,
    FileMissing,
}

impl Default for EnrichmentStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl std::fmt::Display for EnrichmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Must match the TEXT values stored in Postgres exactly
        let s = match self {
            Self::Pending       => "pending",
            Self::Enriching     => "enriching",
            Self::Done          => "done",
            Self::LowConfidence => "low_confidence",
            Self::Unmatched     => "unmatched",
            Self::Failed        => "failed",
            Self::Exhausted     => "exhausted",
            Self::FileMissing   => "file_missing",
        };
        f.write_str(s)
    }
}
```

#### `src/artist.rs`

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Artist {
    pub id:         Uuid,
    pub name:       String,
    pub sort_name:  String,
    pub mbid:       Option<String>,
    pub country:    Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Role of an artist credited on an album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ArtistRole {
    Primary,
    Various,
    Compiler,
    Featuring,
    Remixer,
    Producer,
}

impl Default for ArtistRole {
    fn default() -> Self {
        Self::Primary
    }
}

/// Join record: artist credited on an album.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AlbumArtist {
    pub album_id:   Uuid,
    pub artist_id:  Uuid,
    pub role:       ArtistRole,
    pub position:   i32,
}

/// Join record: artist credited on a track.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TrackArtist {
    pub track_id:   Uuid,
    pub artist_id:  Uuid,
    pub role:       ArtistRole,
    pub position:   i32,
}
```

#### `src/album.rs`

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Album {
    pub id:             Uuid,
    pub title:          String,
    pub release_year:   Option<i32>,
    pub total_tracks:   Option<i32>,
    pub total_discs:    Option<i32>,
    pub mbid:           Option<String>,
    /// Path relative to MEDIA_ROOT, e.g. "Artist/Album/cover.jpg"
    pub cover_art_path: Option<String>,
    pub created_at:     DateTime<Utc>,
}
```

#### `src/track.rs`

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::EnrichmentStatus;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Track {
    pub id:             Uuid,

    // Audio metadata (synchronized to file tags after enrichment)
    pub title:          String,
    pub artist_display: Option<String>,
    pub album_id:       Option<Uuid>,
    pub track_number:   Option<i32>,
    pub disc_number:    Option<i32>,
    pub duration_ms:    Option<i32>,
    pub genre:          Option<String>,
    pub year:           Option<i32>,

    // File identity and change detection (no BLAKE3/file_hash)
    pub audio_fingerprint:  Option<String>,
    pub file_modified_at:   Option<DateTime<Utc>>,
    pub file_size_bytes:    Option<i64>,
    /// Always relative to MEDIA_ROOT
    pub blob_location:      String,

    // Enrichment pipeline state
    pub mbid:                   Option<String>,
    pub acoustid_id:            Option<String>,
    pub enrichment_status:      EnrichmentStatus,
    pub enrichment_confidence:  Option<f32>,
    pub enrichment_attempts:    i32,
    pub enrichment_locked:      bool,
    pub enriched_at:            Option<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // NOTE: search_text and search_vector are generated columns.
    // They are NOT included in INSERT/UPDATE statements.
    // They are read-only and excluded from the Track struct by default.
    // Use TrackSearchRow for queries that need them.
}

/// Lightweight projection used by search queries.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TrackSummary {
    pub id:             Uuid,
    pub title:          String,
    pub artist_display: Option<String>,
    pub album_id:       Option<Uuid>,
    pub duration_ms:    Option<i32>,
    pub blob_location:  String,
}
```

#### `src/user_library.rs`

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Favorite {
    pub id:         Uuid,
    /// Discord user snowflake stored as string
    pub user_id:    String,
    pub track_id:   Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ListenEvent {
    pub id:         Uuid,
    pub user_id:    String,
    pub track_id:   Uuid,
    pub guild_id:   String,
    pub started_at: DateTime<Utc>,
    /// true = played to natural end; false = skipped or interrupted
    pub completed:  bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Playlist {
    pub id:         Uuid,
    pub name:       String,
    pub owner_id:   String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PlaylistItem {
    pub id:          Uuid,
    pub playlist_id: Uuid,
    pub track_id:    Uuid,
    pub position:    i32,
    pub added_at:    DateTime<Utc>,
}
```

---

### Step 3 — `crates/application/`

Delete all existing port files. Replace with the following. All traits use
`#[async_trait]`. Error type is a placeholder `AppError` defined below.
No implementations live in this crate — traits only.

#### `src/lib.rs`

```rust
pub mod error;
pub mod ports;

pub use error::AppError;
```

#### `src/error.rs`

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("external API error: {0}")]
    ExternalApi(String),

    #[error("enrichment error: {0}")]
    Enrichment(String),
}
```

Add `thiserror = "1"` to workspace dependencies.

#### `src/ports/mod.rs`

```rust
pub mod repository;
pub mod search;
pub mod enrichment;
pub mod library;
pub mod file_ops;

pub use repository::{TrackRepository, ArtistRepository, AlbumRepository};
pub use search::TrackSearchPort;
pub use enrichment::{FingerprintPort, AcoustIdPort, MusicBrainzPort, CoverArtPort};
pub use library::LibraryQueryPort;
pub use file_ops::FileTagWriterPort;
```

#### `src/ports/repository.rs`

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use domain::{Album, AlbumArtist, Artist, Favorite, Track, TrackArtist};
use crate::AppError;

#[async_trait]
pub trait TrackRepository: Send + Sync {
    async fn find_by_id(&self, id: Uuid)
        -> Result<Option<Track>, AppError>;

    async fn find_by_fingerprint(&self, fingerprint: &str)
        -> Result<Option<Track>, AppError>;

    async fn find_by_blob_location(&self, location: &str)
        -> Result<Option<Track>, AppError>;

    async fn insert(&self, track: &Track)
        -> Result<Track, AppError>;

    async fn update_file_identity(
        &self,
        id: Uuid,
        file_modified_at: DateTime<Utc>,
        file_size_bytes: i64,
        blob_location: &str,
    ) -> Result<(), AppError>;

    async fn update_fingerprint(&self, id: Uuid, fingerprint: &str)
        -> Result<(), AppError>;

    async fn update_enrichment_status(
        &self,
        id: Uuid,
        status: &domain::EnrichmentStatus,
        attempts: i32,
        enriched_at: Option<DateTime<Utc>>,
    ) -> Result<(), AppError>;

    async fn update_enriched_metadata(
        &self,
        id: Uuid,
        title: &str,
        artist_display: &str,
        album_id: Option<Uuid>,
        genre: Option<&str>,
        year: Option<i32>,
        mbid: Option<&str>,
        acoustid_id: Option<&str>,
        confidence: Option<f32>,
    ) -> Result<(), AppError>;

    /// Used by enrichment orchestrator — FOR UPDATE SKIP LOCKED
    async fn claim_for_enrichment(&self, limit: i64)
        -> Result<Vec<Track>, AppError>;

    /// Startup watchdog: reset stale 'enriching' rows to 'pending'
    async fn reset_stale_enriching(&self)
        -> Result<u64, AppError>;

    /// /rescan --force: reset exhausted + low_confidence to pending
    async fn force_rescan(&self)
        -> Result<u64, AppError>;

    async fn mark_file_missing(&self, blob_location: &str)
        -> Result<(), AppError>;
}

#[async_trait]
pub trait ArtistRepository: Send + Sync {
    async fn find_by_mbid(&self, mbid: &str)
        -> Result<Option<Artist>, AppError>;

    async fn upsert(&self, artist: &Artist)
        -> Result<Artist, AppError>;

    async fn upsert_track_artist(&self, ta: &TrackArtist)
        -> Result<(), AppError>;

    async fn upsert_album_artist(&self, aa: &AlbumArtist)
        -> Result<(), AppError>;
}

#[async_trait]
pub trait AlbumRepository: Send + Sync {
    async fn find_by_mbid(&self, mbid: &str)
        -> Result<Option<Album>, AppError>;

    async fn upsert(&self, album: &Album)
        -> Result<Album, AppError>;

    async fn update_cover_art_path(&self, id: Uuid, path: &str)
        -> Result<(), AppError>;
}
```

#### `src/ports/search.rs`

```rust
use async_trait::async_trait;
use domain::TrackSummary;
use crate::AppError;

#[async_trait]
pub trait TrackSearchPort: Send + Sync {
    /// Hybrid FTS + trigram search. Only returns tracks with
    /// enrichment_status = 'done'.
    async fn search(&self, query: &str, limit: usize)
        -> Result<Vec<TrackSummary>, AppError>;

    /// Autocomplete: prefix match on title and artist_display.
    /// Only returns tracks with enrichment_status = 'done'.
    async fn autocomplete(&self, prefix: &str, limit: usize)
        -> Result<Vec<TrackSummary>, AppError>;
}
```

#### `src/ports/enrichment.rs`

```rust
use async_trait::async_trait;
use std::path::Path;
use crate::AppError;

/// Raw tags read from a file before enrichment.
#[derive(Debug, Clone)]
pub struct RawFileTags {
    pub title:        Option<String>,
    pub artist:       Option<String>,
    pub album:        Option<String>,
    pub year:         Option<i32>,
    pub genre:        Option<String>,
    pub track_number: Option<u32>,
    pub disc_number:  Option<u32>,
    pub duration_ms:  Option<u32>,
}

/// Chromaprint fingerprint result.
#[derive(Debug, Clone)]
pub struct AudioFingerprint {
    pub fingerprint:  String,     // base64-encoded Chromaprint string
    pub duration_secs: u32,
}

/// Best match returned by AcoustID lookup.
#[derive(Debug, Clone)]
pub struct AcoustIdMatch {
    pub recording_mbid: String,
    pub score:          f32,
    pub acoustid_id:    String,
}

/// Recording data returned by MusicBrainz.
#[derive(Debug, Clone)]
pub struct MbRecording {
    pub title:          String,
    pub artist_credits: Vec<MbArtistCredit>,
    pub release_mbid:   String,
    pub release_title:  String,
    pub release_year:   Option<i32>,
    pub genre:          Option<String>,
}

#[derive(Debug, Clone)]
pub struct MbArtistCredit {
    pub artist_mbid: String,
    pub name:        String,
    pub sort_name:   String,
    pub join_phrase: Option<String>, // " feat. ", " & ", etc.
}

#[async_trait]
pub trait FingerprintPort: Send + Sync {
    async fn compute(&self, path: &Path)
        -> Result<(AudioFingerprint, RawFileTags), AppError>;
}

#[async_trait]
pub trait AcoustIdPort: Send + Sync {
    async fn lookup(&self, fp: &AudioFingerprint)
        -> Result<Option<AcoustIdMatch>, AppError>;
}

#[async_trait]
pub trait MusicBrainzPort: Send + Sync {
    async fn fetch_recording(&self, mbid: &str)
        -> Result<MbRecording, AppError>;
}

#[async_trait]
pub trait CoverArtPort: Send + Sync {
    /// Returns raw image bytes if found, None otherwise.
    async fn fetch_front(&self, release_mbid: &str)
        -> Result<Option<bytes::Bytes>, AppError>;

    /// Extract embedded art from file tags. None if no embedded art.
    async fn extract_from_tags(&self, path: &std::path::Path)
        -> Result<Option<bytes::Bytes>, AppError>;
}
```

Add `bytes = "1"` to workspace dependencies.

#### `src/ports/library.rs`

```rust
use async_trait::async_trait;
use uuid::Uuid;
use domain::{Favorite, ListenEvent, Playlist, PlaylistItem, TrackSummary};
use crate::AppError;

pub enum FavoriteStatus { Added, Removed }

#[async_trait]
pub trait LibraryQueryPort: Send + Sync {
    async fn get_favorites(&self, user_id: &str)
        -> Result<Vec<TrackSummary>, AppError>;

    async fn toggle_favorite(&self, user_id: &str, track_id: Uuid)
        -> Result<FavoriteStatus, AppError>;

    async fn record_listen(&self, event: &ListenEvent)
        -> Result<(), AppError>;

    async fn get_listen_history(&self, user_id: &str, limit: usize)
        -> Result<Vec<ListenEvent>, AppError>;

    async fn create_playlist(&self, owner_id: &str, name: &str)
        -> Result<Playlist, AppError>;

    async fn add_to_playlist(&self, playlist_id: Uuid, track_id: Uuid)
        -> Result<PlaylistItem, AppError>;

    async fn get_playlist_tracks(&self, playlist_id: Uuid)
        -> Result<Vec<TrackSummary>, AppError>;

    async fn get_user_playlists(&self, owner_id: &str)
        -> Result<Vec<Playlist>, AppError>;
}
```

#### `src/ports/file_ops.rs`

```rust
use async_trait::async_trait;
use std::path::Path;
use crate::AppError;

/// Enriched metadata to write back into file tags.
#[derive(Debug, Clone)]
pub struct EnrichedTags {
    pub title:          String,
    pub artist_display: String,
    pub album:          Option<String>,
    pub year:           Option<i32>,
    pub genre:          Option<String>,
    pub track_number:   Option<u32>,
    pub disc_number:    Option<u32>,
    pub cover_art:      Option<bytes::Bytes>,
}

/// Result after writing tags back to file.
#[derive(Debug, Clone)]
pub struct TagWriteResult {
    pub new_file_modified_at:   chrono::DateTime<chrono::Utc>,
    pub new_file_size_bytes:    i64,
}

#[async_trait]
pub trait FileTagWriterPort: Send + Sync {
    /// Write enriched tags to the file at `absolute_path` atomically
    /// (tempfile + rename). Returns new mtime and size after write.
    async fn write_tags(
        &self,
        absolute_path: &Path,
        tags: &EnrichedTags,
    ) -> Result<TagWriteResult, AppError>;
}
```

---

### Step 4 — `crates/shared-config/`

Add the following fields to the existing `Config` struct. Do not remove
any fields that existing code depends on.

```rust
use std::path::PathBuf;

pub struct Config {
    // --- existing v1 fields (keep) ---

    // --- v2 additions ---

    /// Absolute path to SMB-mounted music library root.
    pub media_root: PathBuf,

    /// AcoustID API key.
    pub acoustid_api_key: String,

    /// MusicBrainz User-Agent header.
    /// Required format: "AppName/Version (contact@email.com)"
    pub mb_user_agent: String,

    /// PollWatcher poll interval in seconds. Default: 300.
    pub scan_interval_secs: u64,

    /// SMB_READ_SEMAPHORE permit count. Default: 3.
    pub smb_read_concurrency: usize,

    /// Max concurrent Fingerprint Workers. Default: 4.
    pub fingerprint_concurrency: usize,

    /// Max concurrent Cover Art Archive fetches. Default: 4.
    pub cover_art_concurrency: usize,

    /// AcoustID minimum confidence score for 'done'. Default: 0.85.
    pub enrichment_confidence_threshold: f32,

    /// AcoustID no-match retries before 'exhausted'. Default: 3.
    pub unmatched_retry_limit: u32,

    /// Network-error retries before 'exhausted'. Default: 5.
    pub failed_retry_limit: u32,

    /// SQLx pool max connections. Default: 10.
    pub db_pool_size: u32,
}
```

Parsing (using `dotenvy` + `std::env`):

```rust
impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok(); // load .env if present; silently ignore if absent
        Ok(Self {
            // existing fields ...
            media_root: std::env::var("MEDIA_ROOT")
                .map(PathBuf::from)
                .map_err(|_| ConfigError::missing("MEDIA_ROOT"))?,
            acoustid_api_key: std::env::var("ACOUSTID_API_KEY")
                .map_err(|_| ConfigError::missing("ACOUSTID_API_KEY"))?,
            mb_user_agent: std::env::var("MB_USER_AGENT")
                .map_err(|_| ConfigError::missing("MB_USER_AGENT"))?,
            scan_interval_secs: parse_env("SCAN_INTERVAL_SECS", 300)?,
            smb_read_concurrency: parse_env("SMB_READ_CONCURRENCY", 3)?,
            fingerprint_concurrency: parse_env("FINGERPRINT_CONCURRENCY", 4)?,
            cover_art_concurrency: parse_env("COVER_ART_CONCURRENCY", 4)?,
            enrichment_confidence_threshold: parse_env("ENRICHMENT_CONFIDENCE_THRESHOLD", 0.85f32)?,
            unmatched_retry_limit: parse_env("UNMATCHED_RETRY_LIMIT", 3u32)?,
            failed_retry_limit: parse_env("FAILED_RETRY_LIMIT", 5u32)?,
            db_pool_size: parse_env("DB_POOL_SIZE", 10u32)?,
        })
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> Result<T, ConfigError>
where T::Err: std::fmt::Display
{
    match std::env::var(key) {
        Ok(val) => val.parse::<T>().map_err(|e| ConfigError::parse(key, e)),
        Err(_) => Ok(default),
    }
}
```

---

### Step 5 — `crates/adapters-persistence/` Migrations

Create migration files in `migrations/` directory (sqlx migrate convention:
`{timestamp}_{name}.sql`). Use fixed timestamps in ascending order.

#### `migrations/20250001000000_extensions.sql`

```sql
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
```

#### `migrations/20250002000000_core_tables.sql`

```sql
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
            normalize(coalesce(title, ''), 'NFC') || ' ' ||
            normalize(coalesce(artist_display, ''), 'NFC') || ' ' ||
            normalize(coalesce(genre, ''), 'NFC')
        ))
    ) STORED,

    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('music_simple',
            normalize(coalesce(title, ''), 'NFC') || ' ' ||
            normalize(coalesce(artist_display, ''), 'NFC')
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
```

#### `migrations/20250003000000_user_library.sql`

```sql
CREATE TABLE IF NOT EXISTS favorites (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     TEXT NOT NULL,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, track_id)
);

CREATE TABLE IF NOT EXISTS listen_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     TEXT NOT NULL,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    guild_id    TEXT NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed   BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE IF NOT EXISTS playlists (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    owner_id    TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS playlist_items (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    playlist_id     UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id        UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    added_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (playlist_id, position)
);
```

#### `migrations/20250004000000_indexes.sql`

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_fingerprint
    ON tracks(audio_fingerprint)
    WHERE audio_fingerprint IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_tracks_blob_location
    ON tracks(blob_location);

CREATE INDEX IF NOT EXISTS idx_tracks_search_vector
    ON tracks USING GIN(search_vector);

CREATE INDEX IF NOT EXISTS idx_tracks_search_text
    ON tracks USING GIN(search_text gin_trgm_ops);

CREATE INDEX IF NOT EXISTS idx_tracks_enrichment_queue
    ON tracks(enrichment_status, enrichment_attempts, enriched_at)
    WHERE enrichment_locked = false
      AND enrichment_status IN ('pending', 'failed', 'low_confidence', 'unmatched');

CREATE INDEX IF NOT EXISTS idx_favorites_user
    ON favorites(user_id);

CREATE INDEX IF NOT EXISTS idx_listen_events_user
    ON listen_events(user_id, started_at DESC);

CREATE INDEX IF NOT EXISTS idx_listen_events_track
    ON listen_events(track_id);
```

---

### Step 6 — Repository Stubs in `adapters-persistence/`

Implement `TrackRepository` against `sqlx::PgPool`. This pass only requires
the methods needed to verify the schema works — full search and enrichment
queries come in later passes. Stub unneeded methods with `todo!()`.

Methods to implement fully in this pass (no `todo!()`):
- `insert` — full INSERT with all non-generated columns
- `find_by_id`
- `find_by_blob_location`
- `find_by_fingerprint`
- `mark_file_missing`
- `reset_stale_enriching` (startup watchdog)
- `force_rescan` (for `/rescan --force`)

Methods to stub with `todo!()` for now:
- `claim_for_enrichment` — implemented in Pass 2 (enrichment pipeline)
- `update_enrichment_status` — implemented in Pass 2
- `update_enriched_metadata` — implemented in Pass 3 (MusicBrainz)
- `update_file_identity` — implemented in Pass 2

Implement `ArtistRepository::upsert` and `AlbumRepository::upsert` fully.
These are needed early because MusicBrainz results must upsert both.

---

### Step 7 — Drop v1 `media_assets` References

Search the entire codebase for all uses of `media_assets`, `MediaAsset`,
`register_media`, `enqueue_track` (v1 command). For each:

- If it is a Postgres query referencing `media_assets`: the table still
  exists in the DB until a `DROP TABLE media_assets` migration is added.
  **Do not add that migration in this pass.** Leave v1 table in DB untouched.
  Replace Rust code that reads from it with stubs returning empty results.

- If it is a Rust struct or type import: replace with the appropriate v2
  type from `domain/`.

- If it is a Discord command handler that cannot compile without `MediaAsset`:
  stub the handler body to respond with
  `"This command is being updated for v2."` and return Ok.

The goal is zero compile errors, not behavioral correctness of v1 commands.

---

### Step 8 — `apps/bot/` Wiring Update

Update `main.rs` only enough to compile:

1. Call `Config::from_env()` — update to expect new required fields.
   Add the new required fields to your local `.env.example` or test `.env`.

2. Replace any `Arc<dyn v1Port>` wiring that no longer compiles.
   Stub with a `todo!()` implementation if the adapter is not yet written.

3. Run migrations at startup:
   ```rust
   sqlx::migrate!("./crates/adapters-persistence/migrations")
       .run(&pool)
       .await
       .expect("migrations failed");
   ```

4. Add the stale enriching watchdog after migrations:
   ```rust
   track_repo.reset_stale_enriching().await
       .expect("stale enriching watchdog failed");
   ```

---

### Invariants for This Pass

The following invariants from the master document apply to every line of
code written in this pass:

- **No BLAKE3 anywhere.** No `file_hash` column. No `blake3` crate.
- **No `unaccent()` directly in SQL.** Always `immutable_unaccent()`.
- **`blob_location` is always relative to `MEDIA_ROOT`.** Never absolute.
- **No adapter types imported into `domain/` or `application/`.**
- **No `BYTEA` column for cover art.** `cover_art_path TEXT` only.
- **`search_text` and `search_vector` are never written by application code.**
  They are generated columns. Never include them in INSERT or UPDATE statements.

---

### REFERENCE — TeamTI v2 Master Requirements & Design Document

at docs/v2/v2_master.md