# Pass 4: Recommendation Engine, Radio & Discovery Foundation

Implements acoustic analysis (bliss-audio), Last.fm similarity caching, mood-aware radio, materialized affinities, and discovery stats tables per the design spec.

## User Review Required

> [!IMPORTANT]
> **bliss-audio 0.11.2 API deviation**: The design spec references `bliss_audio::Song::from_path()` and `FEATURES_SIZE`, but the actual 0.11.2 API uses:
> - `bliss_audio::decoder::Decoder` trait with `SymphoniaDecoder::song_from_path()` (with `symphonia` feature)
> - `bliss_audio::NUMBER_FEATURES` (not `FEATURES_SIZE`)
> - `song.analysis.as_arr1()` returns an ndarray, and `Analysis` has an `as_vec()` or iteration API
> - The actual vector dimension for Version2 is `AnalysisIndex::COUNT` — I'll verify this at compile time. If it differs from 20, I'll update the migration accordingly before running it.

> [!WARNING]
> **pgvector extension required**: `CREATE EXTENSION IF NOT EXISTS vector` must be available on your PostgreSQL instance. Managed services (Supabase, Neon, RDS) need pgvector enabled on the tier.

> [!IMPORTANT]
> **GPL-3.0 license**: bliss-audio is GPL-3.0-only. This will make the `adapters-analysis` crate GPL-linked. Since this is a private project (not distributed), this is fine. Flagging for awareness.

## Proposed Changes

### Schema Changes (Inline in existing migrations)

#### [MODIFY] [0001_extensions.sql](file:///Users/khanhtimn/Documents/project/teamti/migrations/0001_extensions.sql)
- Add `CREATE EXTENSION IF NOT EXISTS vector;` before other extensions

#### [MODIFY] [0002_core_tables.sql](file:///Users/khanhtimn/Documents/project/teamti/migrations/0002_core_tables.sql)
- Add analysis columns to `tracks`: `analysis_status`, `analysis_attempts`, `analysis_locked`, `analyzed_at`, `bliss_vector vector(N)` (N determined at compile time)

#### [MODIFY] [0003_user_library.sql](file:///Users/khanhtimn/Documents/project/teamti/migrations/0003_user_library.sql)
- Add `similar_artists` table (Last.fm cache)
- Add `user_track_affinities` table
- Add `user_genre_stats` table
- Add `guild_track_stats` table

#### [MODIFY] [0004_indexes.sql](file:///Users/khanhtimn/Documents/project/teamti/migrations/0004_indexes.sql)
- Add analysis queue index, HNSW vector index, similar_artists index, affinity/genre/guild stats indexes

---

### Domain Layer

#### [MODIFY] [lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/domain/src/lib.rs)
- Re-export new types: `AnalysisStatus`, `MoodWeight`, `SimilarArtist`

#### [MODIFY] [track.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/domain/src/track.rs)
- Add `analysis_status`, `analysis_attempts`, `analysis_locked`, `analyzed_at` fields to `Track`
- Note: `bliss_vector` is NOT in the domain `Track` — it's only in the DB/persistence layer

#### [NEW] [analysis.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/domain/src/analysis.rs)
- `AnalysisStatus` enum: `Pending`, `Processing`, `Done`, `Failed`
- `MoodWeight` struct with `acoustic`/`taste` blend weights + named constants

---

### Application Layer

#### [MODIFY] [error.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/error.rs)
- Add `Analysis { kind: AnalysisErrorKind, detail }` variant
- Add `LastFm { kind: LastFmErrorKind, detail }` variant
- Add `AnalysisErrorKind` and `LastFmErrorKind` enums
- Update `kind_str()`, `is_retryable()`, `backoff_hint()`

#### [MODIFY] [events.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/events.rs)
- Add `ToLastFm` event struct (replaces direct MusicBrainz→Lyrics flow)

#### [NEW] [ports/audio_analysis.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/audio_analysis.rs)
- `AudioAnalysisPort` trait with `analyse_track(blob_location) → Vec<f32>`

#### [NEW] [ports/lastfm.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/lastfm.rs)
- `SimilarArtist` struct, `LastFmPort` trait with `get_similar_artists(mbid) → Vec<SimilarArtist>`

#### [MODIFY] [ports/recommendation.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/recommendation.rs)
- Update `recommend()` signature: add `seed_vector`, `user_centroid`, `mood_weight` params
- Add `refresh_affinities(user_id, limit)` method
- Add `update_genre_stats(user_id, genres)` method 
- Add `update_guild_track_stats(guild_id, track_id)` method

#### [MODIFY] [ports/mod.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/mod.rs)
- Register new port modules, add re-exports

#### [MODIFY] [ports/repository.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/repository.rs)
- Add analysis worker methods: `claim_for_analysis()`, `update_analysis_done()`, `update_analysis_failed()`, `reset_stale_analyzing()`
- Add `get_bliss_vector(track_id)` and `compute_user_centroid(user_id)`

#### [NEW] [analysis_worker.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/analysis_worker.rs)
- `AnalysisWorker` struct polling for pending/failed tracks
- Uses `JoinSet` bounded by `ANALYSIS_CONCURRENCY`
- Calls `AudioAnalysisPort::analyse_track()` via `spawn_blocking`

#### [NEW] [lastfm_worker.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/lastfm_worker.rs)
- `LastFmWorker` receives `ToLastFm`, calls `LastFmPort::get_similar_artists()`, emits `ToLyrics`
- Always emits `ToLyrics` even on failure

#### [MODIFY] [musicbrainz_worker.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/musicbrainz_worker.rs)
- Change output channel from `lyrics_tx: Sender<ToLyrics>` to `lastfm_tx: Sender<ToLastFm>`
- Build `ToLastFm` with artist MBIDs collected from upserted artists

#### [MODIFY] [lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/lib.rs)
- Register new modules, add exports

---

### New Crate: `adapters-analysis`

#### [NEW] [Cargo.toml](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-analysis/Cargo.toml)
- Depends on `application`, `domain`, `bliss-audio` (workspace, `symphonia` feature), `tokio`, `tracing`, `thiserror`, `async-trait`

#### [NEW] [src/lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-analysis/src/lib.rs)
- `BlissAnalysisAdapter` implementing `AudioAnalysisPort`
- Compile-time assertion on `NUMBER_FEATURES`
- Uses `SymphoniaDecoder::song_from_path()` in `spawn_blocking`

---

### New Crate: `adapters-lastfm`

#### [NEW] [Cargo.toml](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-lastfm/Cargo.toml)
- Depends on `application`, `reqwest`, `governor`, `serde`, `serde_json`, `tokio`, `tracing`, `thiserror`, `async-trait`, `nonzero_ext`

#### [NEW] [src/lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-lastfm/src/lib.rs)
- `LastFmAdapter` implementing `LastFmPort`
- 4 req/sec rate limiter (governor)
- Calls `artist.getSimilar` endpoint

#### [NEW] [src/response.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-lastfm/src/response.rs)
- Serde deserialization structs for Last.fm JSON responses

---

### Persistence Layer

#### [MODIFY] [Cargo.toml](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-persistence/Cargo.toml)
- Add `pgvector` crate for `sqlx` vector type support

#### [MODIFY] [recommendation_repository.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-persistence/src/repositories/recommendation_repository.rs)
- **Replace** Pass 3 CTE with Pass 4 multi-factor scoring CTE (acoustic, taste, favourites, lastfm)
- Implement `refresh_affinities()`, `update_genre_stats()`, `update_guild_track_stats()`
- Updated `recommend()` signature with vector params and mood weight

#### [MODIFY] [track_repository.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-persistence/src/repositories/track_repository.rs)
- Add `claim_for_analysis()`, `update_analysis_done()`, `update_analysis_failed()`, `reset_stale_analyzing()`, `get_bliss_vector()`, `compute_user_centroid()`

---

### Voice Adapter (Lifecycle)

#### [MODIFY] [lifecycle.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-voice/src/lifecycle.rs)
- Add `AffinityUpdate { user_id }` variant to `TrackLifecycleEvent`

---

### Discord Adapter (Lifecycle Worker)

#### [MODIFY] [lifecycle_worker.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-discord/src/lifecycle_worker.rs)
- Handle `AffinityUpdate` event → call `RecommendationPort::refresh_affinities()`
- On `TrackEnded` (completed), emit `AffinityUpdate` + call genre/guild stats updates
- Update `RadioRefillNeeded` to compute user centroid and seed vector, pass mood weight
- Update `recommend()` call to match new signature

---

### Config & Bot

#### [MODIFY] [shared-config/src/lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/shared-config/src/lib.rs)
- Add `lastfm_api_key: Option<String>`, `analysis_concurrency: usize`, `analysis_poll_secs: u64`

#### [MODIFY] [.env.example](file:///Users/khanhtimn/Documents/project/teamti/.env.example)
- Add `LASTFM_API_KEY=`, `ANALYSIS_CONCURRENCY=4`, `ANALYSIS_POLL_SECS=30`

#### [MODIFY] [Cargo.toml (root)](file:///Users/khanhtimn/Documents/project/teamti/Cargo.toml)
- Add `bliss-audio` workspace dependency

#### [MODIFY] [bot/Cargo.toml](file:///Users/khanhtimn/Documents/project/teamti/apps/bot/Cargo.toml)
- Add `adapters-analysis`, `adapters-lastfm` dependencies

#### [MODIFY] [bot/src/main.rs](file:///Users/khanhtimn/Documents/project/teamti/apps/bot/src/main.rs)
- Instantiate `BlissAnalysisAdapter`, `LastFmAdapter`
- Spawn `AnalysisWorker`
- Wire `ToLastFm` channel between MusicBrainz and LastFm workers
- Handle `LastFm → Lyrics` channel chain
- Pass new ports to lifecycle worker
- Reset stale analyzing tracks at startup

## Open Questions

> [!IMPORTANT]
> **pgvector installation**: Is pgvector already installed on your PostgreSQL instance? The migration will fail if `CREATE EXTENSION vector` is not available. Need to confirm before running the schema pipeline.

> [!NOTE]
> **Recommendation CTE complexity**: The Pass 4 CTE uses `<->` (L2 distance) operator from pgvector and window functions. Since `sqlx::query!` macro validates against the DB at compile time, and pgvector types aren't natively supported by sqlx, I'll use `sqlx::query_as` with raw SQL and the `pgvector` crate for Rust type mapping. This is a pragmatic deviation from the `query!` macro pattern used elsewhere.

## Verification Plan

### Automated Tests
```bash
# Schema + pgvector
cargo sqlx database drop -f -y && cargo sqlx database create
cargo sqlx migrate run
cargo sqlx prepare --workspace
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace
```

### Manual Verification
- Start bot, watch for analysis worker polling logs
- After ~60s: `SELECT count(*) FROM tracks WHERE bliss_vector IS NOT NULL;`
- After enrichment: `SELECT count(*) FROM similar_artists WHERE source_mbid = '<mbid>';`
- `/radio` with different energy seeds, observe queue character
- After listen: check `user_track_affinities`, `user_genre_stats`, `guild_track_stats`
