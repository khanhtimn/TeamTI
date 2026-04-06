# TeamTI v3 — Pass 4 Design Spec
## Recommendation Engine, Radio, & Discovery Foundation

> All decisions are locked. This is the authoritative reference for
> Pass 4. Attach alongside current migration files, `main.rs`, the
> existing pipeline workers, and the Pass 3 output before sending
> to the agent.

---

## Decision Log

| Topic | Decision |
|-------|----------|
| Vector backend | pgvector (PostgreSQL extension) — no Qdrant, no external DB |
| Acoustic analysis | bliss-audio with Symphonia feature flag (pure Rust, no FFmpeg) |
| Distance metric | Euclidean (`<->`) — matches bliss's native distance function |
| Last.fm | Yes — artist.getSimilar, cached in DB at enrichment time, never at query time |
| Affinity table | Decomposed scores (favourites, acoustic, taste, lastfm + combined) |
| Affinity updates | Event-driven: recompute top-K affinities on listen completion and favourite add |
| Analysis status | New `analysis_status` column (traceable, same pattern as enrichment_status) |
| Analysis concurrency | Low-priority thread pool, configurable, default = 4 |
| NAS failure | Mark analysis_failed, increment attempts, retry on next startup |
| Last.fm pipeline position | New stage between MusicBrainz and LRCLIB |
| Radio blend | Mood-aware: high-energy seed → weight acoustic; low-energy → weight taste |
| Recommendation priority | 1. Favourites signal, 2. Acoustic, 3. Taste, 4. Last.fm |
| Discovery data | Top genres, track affinities, server popularity, artist similarity |
| Decoder | Symphonia (pure Rust) |
| Qdrant/linfa/burn/polars | Not used — pgvector at this scale is sufficient |

---

## Schema Changes

All changes are inline in existing migration files. No new migration files.
After editing, run: `cargo sqlx database drop && cargo sqlx database create && cargo sqlx migrate run && cargo sqlx prepare --workspace`

### `0001_extensions.sql` — add pgvector

```sql
CREATE EXTENSION IF NOT EXISTS vector;
```

Add before all table definitions — pgvector must exist before any
`vector(N)` column can be created.

### `0002_core_tables-2.sql` — add analysis columns to tracks

Add to the `CREATE TABLE tracks` definition:

```sql
-- ── Audio analysis (bliss-audio) ──────────────────────────────
-- Mirrors the enrichment_status pattern for traceability.
-- analysis_status = 'pending'    → not yet analysed
--                  'processing'  → currently locked by analysis worker
--                  'done'        → bliss_vector is populated
--                  'failed'      → analysis attempt failed (file unreadable, decode error)
-- Triggers on all tracks regardless of enrichment_status.
analysis_status   TEXT NOT NULL DEFAULT 'pending'
                      CHECK (analysis_status IN ('pending', 'processing', 'done', 'failed')),
analysis_attempts INTEGER NOT NULL DEFAULT 0,
analysis_locked   BOOLEAN NOT NULL DEFAULT false,
analyzed_at       TIMESTAMPTZ,

-- 20-dimensional bliss-audio feature vector (Euclidean distance space).
-- Verify dimension with `bliss_audio::FEATURES_SIZE` at implementation time.
-- NULL until analysis_status = 'done'.
-- Query with: ORDER BY bliss_vector <-> $seed_vector (L2 distance)
bliss_vector      vector(20),
```

### `0003_user_library-3.sql` — new tables

Add after the existing listen_events table:

```sql
-- ── Last.fm artist similarity cache ───────────────────────────
-- Populated once per artist at enrichment time by LastFmWorker.
-- Never populated at recommendation time — no live API calls in hot path.
-- source_mbid and similar_mbid are MusicBrainz Artist IDs.
-- An artist pair is stored even if similar_mbid is not yet in our artists table.
CREATE TABLE IF NOT EXISTS similar_artists (
    source_mbid      TEXT NOT NULL,
    similar_mbid     TEXT NOT NULL,
    similarity_score REAL NOT NULL CHECK (similarity_score >= 0 AND similarity_score <= 1),
    fetched_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_mbid, similar_mbid)
);

-- ── Materialized user-track affinities ────────────────────────
-- Top-K tracks recommended for each user, with decomposed signal scores.
-- Decomposed scores allow reweighting without recomputing raw signals.
-- Populated/updated eagerly on listen completion and favourite events.
-- track_id is a track the user has NOT yet heard (or rarely heard),
-- scored as a recommendation candidate.
CREATE TABLE IF NOT EXISTS user_track_affinities (
    user_id          TEXT NOT NULL,
    track_id         UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    favourites_score REAL NOT NULL DEFAULT 0,   -- similarity to user's favourites centroid
    acoustic_score   REAL NOT NULL DEFAULT 0,   -- similarity to user's taste centroid vector
    taste_score      REAL NOT NULL DEFAULT 0,   -- genre + artist affinity from listen history
    lastfm_score     REAL NOT NULL DEFAULT 0,   -- similar_artists graph proximity
    combined_score   REAL NOT NULL DEFAULT 0,   -- weighted blend (updated with weights)
    computed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, track_id)
);

-- ── Web portal: top genres per user per period ────────────────
-- Populated/updated on listen event close.
-- period_start / period_end define the time window (e.g., current calendar month).
CREATE TABLE IF NOT EXISTS user_genre_stats (
    user_id      TEXT NOT NULL,
    genre        TEXT NOT NULL,
    play_count   INTEGER NOT NULL DEFAULT 0,
    period_start TIMESTAMPTZ NOT NULL,
    period_end   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, genre, period_start)
);

-- ── Web portal: server-wide track popularity ──────────────────
-- Populated/updated on listen event close (completed = true only).
CREATE TABLE IF NOT EXISTS guild_track_stats (
    guild_id     TEXT NOT NULL,
    track_id     UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    play_count   INTEGER NOT NULL DEFAULT 0,
    period_start TIMESTAMPTZ NOT NULL,
    period_end   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (guild_id, track_id, period_start)
);
```

### `0004_indexes-4.sql` — new indexes

```sql
-- Analysis worker queue (mirrors idx_tracks_enrichment_queue)
CREATE INDEX IF NOT EXISTS idx_tracks_analysis_queue
ON tracks(analysis_status, analysis_attempts, analyzed_at)
WHERE analysis_locked = false
  AND analysis_status IN ('pending', 'failed');

-- Vector similarity: used by pgvector ANN queries
-- For 20-dim vectors at 50k rows, HNSW is optional — add if query
-- latency exceeds 5ms under load. For now, the index is a no-op
-- if pgvector uses flat (exact) search at this scale.
CREATE INDEX IF NOT EXISTS idx_tracks_bliss_vector
ON tracks USING hnsw (bliss_vector vector_l2_ops);

-- Last.fm lookup
CREATE INDEX IF NOT EXISTS idx_similar_artists_source
ON similar_artists(source_mbid);

-- Affinity recommendations for a user, ranked by combined score
CREATE INDEX IF NOT EXISTS idx_user_track_affinities_user
ON user_track_affinities(user_id, combined_score DESC);

-- Discovery page: genre trends
CREATE INDEX IF NOT EXISTS idx_user_genre_stats_user
ON user_genre_stats(user_id, period_start DESC, play_count DESC);

-- Discovery page: server popularity
CREATE INDEX IF NOT EXISTS idx_guild_track_stats_guild
ON guild_track_stats(guild_id, period_start DESC, play_count DESC);
```

---

## New Crates

### `crates/adapters-analysis`

```
crates/adapters-analysis/
├── Cargo.toml
└── src/
    └── lib.rs      BlissAnalysisAdapter: implements AudioAnalysisPort
```

#### `Cargo.toml`

```toml
[package]
name    = "adapters-analysis"
version = "0.1.0"
edition = "2021"

[dependencies]
application = { path = "../application" }
domain      = { path = "../domain" }
bliss-audio = { workspace = true, features = ["symphonia"] }
tokio       = { workspace = true, features = ["rt"] }
tracing     = { workspace = true }
thiserror   = { workspace = true }
async-trait = { workspace = true }
```

In workspace `Cargo.toml`:
```toml
bliss-audio = { version = "0.11", default-features = false, features = ["symphonia"] }
```

Verify `0.11` is the latest stable before using. Check `bliss_audio::FEATURES_SIZE`
at implementation time — use this constant as the vector dimension everywhere
rather than hardcoding `20`. If `FEATURES_SIZE != 20`, update the SQL migration
accordingly before running it.

#### Implementation pattern

```rust
pub struct BlissAnalysisAdapter {
    media_root: PathBuf,
}

#[async_trait]
impl AudioAnalysisPort for BlissAnalysisAdapter {
    async fn analyse_track(
        &self,
        blob_location: &str,
    ) -> Result<Vec<f32>, AppError> {
        let path = self.media_root.join(blob_location);

        // bliss analysis is CPU-bound and synchronous — run in
        // spawn_blocking to avoid blocking the async executor.
        tokio::task::spawn_blocking(move || {
            let song = bliss_audio::Song::from_path(&path)
                .map_err(|e| AppError::Analysis {
                    kind:   AnalysisErrorKind::DecodeFailed,
                    detail: e.to_string(),
                })?;

            Ok(song.analysis.as_slice().to_vec())
        })
        .await
        .map_err(|e| AppError::Analysis {
            kind:   AnalysisErrorKind::TaskPanicked,
            detail: e.to_string(),
        })?
    }
}
```

The `analysis` field on `bliss_audio::Song` returns `&Analysis` which
implements `AsRef<[f32]>` — confirm this at implementation time against
the actual 0.11 API. The vector dimension is `bliss_audio::FEATURES_SIZE`.

### `crates/adapters-lastfm`

```
crates/adapters-lastfm/
├── Cargo.toml
└── src/
    ├── lib.rs          LastFmAdapter: implements LastFmPort
    └── response.rs     Deserialization structs for Last.fm JSON
```

Base URL: `https://ws.audioscrobbler.com/2.0/`

`artist.getSimilar` parameters: `method=artist.getSimilar&mbid={mbid}&api_key={key}&format=json&limit=50`
`track.getSimilar` parameters: `method=track.getSimilar&mbid={mbid}&api_key={key}&format=json&limit=25`

Rate limiter: 4 req/sec (conservative — Last.fm's unofficial limit is ~5/sec).
Reuse the existing `governor` rate limiter pattern from `adapters-musicbrainz`.

`User-Agent: TeamTI/0.1.0`

On 4xx/5xx: return `AppError::LastFm { kind: LastFmErrorKind::ApiError }` — non-fatal,
Last.fm similarity is optional enrichment, not blocking.

---

## AppError — New Variants

```rust
// Analysis errors
#[error("audio analysis error ({kind}): {detail}")]
Analysis { kind: AnalysisErrorKind, detail: String },

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnalysisErrorKind {
    #[error("file not found")]      FileNotFound,
    #[error("decode failed")]       DecodeFailed,
    #[error("task panicked")]       TaskPanicked,
    #[error("vector store failed")] StoreFailed,
}

// Last.fm errors
#[error("last.fm error ({kind}): {detail}")]
LastFm { kind: LastFmErrorKind, detail: String },

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LastFmErrorKind {
    #[error("api error")]       ApiError,
    #[error("not found")]       NotFound,
    #[error("rate limited")]    RateLimited,
    #[error("response invalid")] InvalidResponse,
}
```

Both are non-fatal — log as `warn`, do not fail the enrichment pipeline.

---

## New Application Ports

### `ports/audio_analysis.rs`

```rust
#[async_trait]
pub trait AudioAnalysisPort: Send + Sync {
    /// Analyse a track and return its bliss feature vector.
    /// blob_location is relative to MEDIA_ROOT.
    /// Returns AppError::Analysis on file-not-found or decode failure.
    async fn analyse_track(
        &self,
        blob_location: &str,
    ) -> Result<Vec<f32>, AppError>;
}
```

### `ports/lastfm.rs`

```rust
pub struct SimilarArtist {
    pub mbid:             String,
    pub name:             String,
    pub similarity_score: f32,
}

pub struct SimilarTrack {
    pub mbid:             Option<String>,
    pub title:            String,
    pub artist_mbid:      Option<String>,
    pub similarity_score: f32,
}

#[async_trait]
pub trait LastFmPort: Send + Sync {
    /// Fetch artists similar to the given MusicBrainz artist MBID.
    /// Returns an empty Vec if the artist is unknown to Last.fm.
    async fn get_similar_artists(
        &self,
        artist_mbid: &str,
    ) -> Result<Vec<SimilarArtist>, AppError>;
}
```

### `ports/recommendation.rs` — update signature

Add `user_centroid: Option<Vec<f32>>` parameter to `recommend()`:

```rust
async fn recommend(
    &self,
    user_id:        &str,
    seed_track_id:  Option<Uuid>,
    seed_vector:    Option<Vec<f32>>,   // bliss vector of seed track
    user_centroid:  Option<Vec<f32>>,   // weighted avg of user's liked track vectors
    mood_weight:    MoodWeight,         // acoustic vs taste bias
    exclude:        &[Uuid],
    limit:          usize,
) -> Result<Vec<TrackSummary>, AppError>;

/// Controls the acoustic/taste blend in mood-aware radio.
pub struct MoodWeight {
    pub acoustic: f32,  // 0.0–1.0
    pub taste:    f32,  // 0.0–1.0, typically (1.0 - acoustic)
}

impl MoodWeight {
    /// High-energy seed: weight acoustic similarity more.
    pub const ACOUSTIC_DOMINANT: Self = Self { acoustic: 0.70, taste: 0.30 };
    /// Low-energy seed: weight taste/history more.
    pub const TASTE_DOMINANT: Self    = Self { acoustic: 0.35, taste: 0.65 };
    /// Balanced blend.
    pub const BALANCED: Self          = Self { acoustic: 0.50, taste: 0.50 };
}
```

---

## New Workers

### `AnalysisWorker`

Lives in `crates/application/src/analysis_worker.rs`.

```
Loop:
  1. SELECT tracks WHERE analysis_status IN ('pending', 'failed')
       AND analysis_locked = false
       ORDER BY analysis_attempts ASC, created_at ASC
       LIMIT batch_size
       FOR UPDATE SKIP LOCKED
  2. Set analysis_locked = true for each fetched row
  3. For each track (in separate low-priority spawn_blocking tasks, max ANALYSIS_CONCURRENCY):
     a. Call AudioAnalysisPort::analyse_track(blob_location)
     b. On success: UPDATE bliss_vector = $vec, analysis_status = 'done',
                           analyzed_at = now(), analysis_locked = false
     c. On AppError::Analysis { FileNotFound }: UPDATE analysis_status = 'failed',
                                                       analysis_attempts += 1,
                                                       analysis_locked = false
        log WARN — NAS may be down, will retry
     d. On other errors: same as (c) but log ERROR
  4. Sleep ANALYSIS_POLL_INTERVAL between batches
```

Config values (in `shared-config`):
```rust
/// Maximum concurrent bliss analysis tasks.
/// Each task is CPU-bound (~4–6s per track). Default 4 avoids
/// saturating the host CPU while enrichment pipeline runs.
pub analysis_concurrency: usize,  // env: ANALYSIS_CONCURRENCY, default: 4

/// Polling interval between analysis worker batches (seconds).
pub analysis_poll_secs: u64,      // env: ANALYSIS_POLL_SECS, default: 30
```

The analysis worker spawns independently of the enrichment pipeline.
It runs from startup and never exits. It is NOT gated on enrichment_status —
every track with an audio file gets analysed.

### `LastFmWorker`

Lives in `crates/application/src/lastfm_worker.rs`.

Pipeline position: receives `ToLastFm` event from `MusicBrainzWorker`,
emits `ToLyrics` when done.

```
On ToLastFm { track_id, artist_mbids, ... }:
  1. For each artist_mbid in artist_mbids:
     a. Check: SELECT 1 FROM similar_artists WHERE source_mbid = $mbid LIMIT 1
     b. If already cached: skip (don't re-fetch)
     c. If not cached: call LastFmPort::get_similar_artists(mbid)
        On success: INSERT INTO similar_artists (source_mbid, similar_mbid,
                    similarity_score, fetched_at) ON CONFLICT DO UPDATE
                    SET similarity_score = EXCLUDED.similarity_score,
                        fetched_at = now()
        On error: log WARN, continue (non-fatal)
  2. Emit ToLyrics (always — Last.fm failure does not block the pipeline)
```

The `ToLastFm` event carries artist MBIDs from the MusicBrainz result.
Only artists with a non-null MBID are looked up (niche/local tracks without
MB IDs simply have no Last.fm data — expected and correct).

---

## Recommendation Engine: Full Scoring

All scoring runs in a single PostgreSQL CTE. No per-row Rust computation
in the hot path.

### User centroid vector (computed at affinity update time, not query time)

```sql
-- Weighted average of bliss vectors for tracks the user likes.
-- Liked = favourited (weight 2.0) OR completed listen (weight 1.0).
-- Result: one vector representing user's "acoustic taste".
SELECT
    AVG(t.bliss_vector * CASE WHEN f.user_id IS NOT NULL THEN 2.0 ELSE 1.0 END)
FROM tracks t
LEFT JOIN favorites f ON f.track_id = t.id AND f.user_id = $1
JOIN listen_events le ON le.track_id = t.id
    AND le.user_id = $1 AND le.completed = true
WHERE t.bliss_vector IS NOT NULL
```

Store as a separate `user_acoustic_centroid` column or compute inline.
For simplicity in Pass 4, compute and pass as `$user_centroid` parameter.

### Recommendation CTE

```sql
WITH
-- 1. Seed track's bliss vector (for acoustic similarity)
seed AS (
    SELECT bliss_vector FROM tracks WHERE id = $seed_track_id
),

-- 2. User's top genres from completed listens
user_top_genres AS (
    SELECT UNNEST(t.genres) AS genre, COUNT(*) AS cnt
    FROM listen_events le
    JOIN tracks t ON t.id = le.track_id
    WHERE le.user_id = $user_id AND le.completed = true
    GROUP BY genre ORDER BY cnt DESC LIMIT 10
),

-- 3. User's top artist MBIDs from completed listens
user_top_artists AS (
    SELECT a.mbid, COUNT(*) AS cnt
    FROM listen_events le
    JOIN track_artists ta ON ta.track_id = le.track_id
    JOIN artists a ON a.id = ta.artist_id
    WHERE le.user_id = $user_id AND le.completed = true AND a.mbid IS NOT NULL
    GROUP BY a.mbid ORDER BY cnt DESC LIMIT 20
),

-- 4. Candidate scoring
candidates AS (
    SELECT
        t.id,
        t.title,
        t.artist_display,
        t.bliss_vector,

        -- Acoustic score: similarity to seed (higher = closer)
        -- 0.0 if either vector is NULL (track not yet analysed)
        CASE
            WHEN t.bliss_vector IS NOT NULL AND (SELECT bliss_vector FROM seed) IS NOT NULL
            THEN 1.0 / (1.0 + (t.bliss_vector <-> (SELECT bliss_vector FROM seed)))
            ELSE 0.0
        END AS acoustic_score,

        -- Taste score: genre + artist affinity
        (
            COALESCE(
                (SELECT SUM(utg.cnt)::float / NULLIF(SUM(utg.cnt) OVER (), 0)
                 FROM user_top_genres utg
                 WHERE utg.genre = ANY(t.genres)
                 LIMIT 1), 0.0
            ) * 0.5
          + COALESCE(
                (SELECT MAX(uta.cnt)::float / NULLIF(MAX(uta.cnt) OVER (), 0)
                 FROM track_artists ta2
                 JOIN artists a2 ON a2.id = ta2.artist_id
                 JOIN user_top_artists uta ON uta.mbid = a2.mbid
                 WHERE ta2.track_id = t.id
                 LIMIT 1), 0.0
            ) * 0.5
        ) AS taste_score,

        -- Favourites score: similarity to user's acoustic centroid
        -- ($user_centroid is the pre-computed centroid vector passed in)
        CASE
            WHEN t.bliss_vector IS NOT NULL AND $user_centroid IS NOT NULL
            THEN 1.0 / (1.0 + (t.bliss_vector <-> $user_centroid::vector))
            ELSE 0.0
        END AS favourites_score,

        -- Last.fm score: are this track's artists similar to user's top artists?
        COALESCE(
            (SELECT MAX(sa.similarity_score)
             FROM track_artists ta3
             JOIN artists a3 ON a3.id = ta3.artist_id
             JOIN similar_artists sa ON sa.similar_mbid = a3.mbid
             JOIN user_top_artists uta2 ON uta2.mbid = sa.source_mbid
             WHERE ta3.track_id = t.id), 0.0
        ) AS lastfm_score

    FROM tracks t
    WHERE t.id != $seed_track_id
      AND t.id != ALL($exclude::uuid[])
),

-- 5. Apply weights + mood bias
scored AS (
    SELECT
        id, title, artist_display,
        (
            favourites_score * $w_fav
          + acoustic_score   * ($w_acoustic * $mood_acoustic)
          + taste_score      * ($w_taste    * $mood_taste)
          + lastfm_score     * $w_lastfm
        ) AS total_score
    FROM candidates
)

SELECT id, title, artist_display
FROM scored
ORDER BY total_score DESC
LIMIT $limit
```

Weight parameters passed at query time (never hardcoded in SQL):
- `$w_fav = 0.35`
- `$w_acoustic = 0.30`
- `$w_taste = 0.25`
- `$w_lastfm = 0.10`
- `$mood_acoustic` and `$mood_taste` from `MoodWeight` struct

These are `application` layer constants, not schema values. Pass 4.1
tunes them against real listening data.

### Mood detection (in Rust, before calling the query)

```rust
/// Detect energy level of the seed track from its bliss vector.
/// Returns MoodWeight based on energy tier.
///
/// bliss vector layout (verify indices with bliss_audio::analysis docs):
///   [0]  = tempo
///   [1]  = zcr (zero-crossing rate)
///   [2..] = spectral features
///
/// High energy = high tempo + high spectral centroid
fn mood_weight_for_track(bliss_vector: &[f32]) -> MoodWeight {
    // Indices must be verified against bliss_audio source at implementation.
    // These are indicative — confirm with bliss_audio::TEMPO_IDX etc.
    let tempo        = bliss_vector.get(0).copied().unwrap_or(0.0);
    let spectral_avg = bliss_vector.iter().skip(2).take(4).sum::<f32>() / 4.0;
    let energy       = (tempo + spectral_avg) / 2.0;

    // Thresholds are initial guesses — Pass 4.1 calibrates these
    // against actual bliss vectors from the library.
    if energy > 0.6 {
        MoodWeight::ACOUSTIC_DOMINANT
    } else if energy < 0.3 {
        MoodWeight::TASTE_DOMINANT
    } else {
        MoodWeight::BALANCED
    }
}
```

---

## Affinity Update Pipeline

After `UserLibraryPort::close_listen_event` (completed listen) and
`UserLibraryPort::add_favourite`, the lifecycle worker dispatches an
`AfffinityUpdate { user_id }` event via the existing lifecycle channel:

```rust
// Add to TrackLifecycleEvent:
AfffinityUpdate { user_id: String },
```

The lifecycle worker handles it by calling a new port method:

```rust
// In RecommendationPort:
async fn refresh_affinities(
    &self,
    user_id: &str,
    limit:   usize,
) -> Result<(), AppError>;
```

Implementation:
1. Compute user centroid vector (SQL aggregate over favourites + completed listens)
2. Run the recommendation CTE with seed_track_id = None, limit = 50
3. UPSERT top-50 results into `user_track_affinities`
4. Prune rows older than 50 for this user (keep only the current top-50)

Also update `user_genre_stats` and `guild_track_stats` at listen close:

```rust
// After close_listen_event, if completed:
// 1. INSERT INTO user_genre_stats (user_id, genre, play_count, period_start, period_end)
//    SELECT $user_id, unnest(genres), 1, date_trunc('month', now()), ...
//    ON CONFLICT (user_id, genre, period_start) DO UPDATE SET play_count += 1
//
// 2. INSERT INTO guild_track_stats (guild_id, track_id, play_count, ...)
//    ON CONFLICT DO UPDATE SET play_count += 1
```

---

## Pipeline Stage: `ToLastFm`

Current pipeline chain:
```
ToMusicBrainz → ToLyrics → ToCoverArt → ToTagWriter
```

New chain:
```
ToMusicBrainz → ToLastFm → ToLyrics → ToCoverArt → ToTagWriter
```

`MusicBrainzWorker` emits `ToLastFm` instead of `ToLyrics`.
`LastFmWorker` emits `ToLyrics` on completion (success or failure).

`ToLastFm` event carries:
```rust
pub struct ToLastFm {
    // All existing ToLyrics fields (pass-through):
    pub track_id:      Uuid,
    pub blob_location: String,
    pub title:         String,
    pub artist_name:   String,
    pub album_name:    Option<String>,
    pub duration_secs: u32,
    // New:
    pub artist_mbids:  Vec<String>,   // from track_artists, non-null MBIDs only
}
```

---

## Config Additions (`shared-config`)

```rust
/// Last.fm API key. Required for Last.fm similarity features.
/// If None, LastFmWorker logs a warning and passes all events through
/// to the next stage without making any API calls.
pub lastfm_api_key: Option<String>,   // env: LASTFM_API_KEY

/// Max concurrent bliss audio analysis tasks.
pub analysis_concurrency: usize,      // env: ANALYSIS_CONCURRENCY, default: 4

/// Seconds between analysis worker polling loops.
pub analysis_poll_secs: u64,          // env: ANALYSIS_POLL_SECS, default: 30
```

`.env.example` additions:
```
# Last.fm API key (free at https://www.last.fm/api/account/create)
# Optional — omit to disable Last.fm similarity features.
LASTFM_API_KEY=

# Bliss audio analysis settings
ANALYSIS_CONCURRENCY=4
ANALYSIS_POLL_SECS=30
```

---

## Crates Affected

| Crate | Change |
|-------|--------|
| `crates/domain` | New types: `SimilarArtist`, `MoodWeight`, `AnalysisStatus` |
| `crates/application` | New ports: `AudioAnalysisPort`, `LastFmPort`; new workers: `AnalysisWorker`, `LastFmWorker`; updated `RecommendationPort`; new `AppError` variants; new pipeline event `ToLastFm`; `AfffinityUpdate` lifecycle event |
| `crates/adapters-analysis` | **New crate** — bliss-audio/Symphonia implementation |
| `crates/adapters-lastfm` | **New crate** — Last.fm HTTP adapter |
| `crates/adapters-persistence` | `PgRecommendationRepository`: updated scoring CTE, new `refresh_affinities`, genre/guild stats update methods |
| `apps/bot` | Instantiate new adapters, spawn `AnalysisWorker`, wire `ToLastFm` into pipeline, handle `AfffinityUpdate` lifecycle events |

---

## Verification Plan

```bash
# Schema + pgvector
cargo sqlx database drop && cargo sqlx database create
cargo sqlx migrate run
# Verify: psql -c "\dx" | grep vector
cargo sqlx prepare --workspace
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# Manual — analysis pipeline
# Start bot, watch for analysis worker polling logs
# After ~30s, check: SELECT count(*) FROM tracks WHERE bliss_vector IS NOT NULL;
# Should increment as tracks are analysed

# Manual — Last.fm caching
# After a track enrichment completes:
# SELECT count(*) FROM similar_artists WHERE source_mbid = '<artist_mbid>';
# Should be > 0 for any mainstream artist

# Manual — improved radio
# /play a high-energy track → /radio
# Let queue drain → verify new tracks are energetically similar to seed
# /play a low-energy track → /radio
# Let queue drain → verify new tracks match user taste/genre more than energy

# Manual — affinity updates
# /play a track to completion → check user_track_affinities for updated rows
# /favourite add → check user_track_affinities refreshed

# Manual — web portal data (query directly)
# SELECT * FROM user_genre_stats WHERE user_id = '<your_discord_id>'
#   ORDER BY play_count DESC;
# SELECT * FROM guild_track_stats WHERE guild_id = '<your_guild_id>'
#   ORDER BY play_count DESC LIMIT 10;
```

---

## Note on bliss Vector Dimensions

The migration uses `vector(20)`. Before running the migration, verify:

```rust
// In adapters-analysis/src/lib.rs, add a compile-time assertion:
const _: () = assert!(
    bliss_audio::FEATURES_SIZE == 20,
    "bliss FEATURES_SIZE has changed — update vector(N) in migration"
);
```

If `FEATURES_SIZE != 20`, update the migration and all SQL references
before running `cargo sqlx migrate run`. This is a one-time check.
