# TeamTI v3 — Audit Pass 4.1
## Recommendation Engine & Analysis Pipeline: Correctness & Deployment Review

> **Scope.** Full review of the Pass 4 output across all 10 phases.
> Fix all Critical and Major findings. Apply Optimizations unless they
> conflict with the pure-Rust / no-system-dependency constraint.
> Self-Explore items require reading the implementation — fix if broken,
> document if intentionally accepted.
>
> **Attach:** all Pass 4 output files before sending.
> **Run first:** `cargo build --workspace 2>&1 | grep -E "^error|warning"`
> and report actual output. The walkthrough says "zero errors" but
> does not address warnings.

---

## Findings Index

| ID | Severity | Area | Title |
|----|----------|------|-------|
| C1 | Critical | adapters-analysis | `aubio-sys` C FFI backend used instead of Symphonia |
| C2 | Critical | Schema / adapters-analysis | `vector(23)` — compile-time dimension assertion missing |
| C3 | Critical | AnalysisWorker | Stuck `analysis_locked = true` rows after crash — no startup cleanup |
| M1 | Major | Recommendation | Mood detection uses hardcoded bliss vector indices — unverified |
| M2 | Major | Recommendation | User centroid recomputed on every recommendation call |
| M3 | Major | Persistence | `user_genre_stats` / `guild_track_stats` period boundaries — must be calendar-aligned |
| M4 | Major | Persistence | `ON CONFLICT` stats upsert — verify correct `EXCLUDED` vs table reference in SET |
| M5 | Major | Lifecycle | `AffinityUpdate` may fire on skipped tracks, not only completed listens |
| O1 | Optim. | AnalysisWorker | Lock acquisition — verify `FOR UPDATE SKIP LOCKED` is used |
| O2 | Optim. | LastFmWorker | Per-artist cache check — verify single query, not per-MBID round-trips |
| O3 | Optim. | Persistence | `user_track_affinities` pruning — old rows accumulate without bound |
| S1 | Explore | Deployment | `pgvector` host installation — document requirement and failure mode |
| S2 | Explore | adapters-analysis | bliss-audio v2 `analysis` field access — verify API matches version |
| S3 | Explore | LastFmWorker | Last.fm graceful degradation when `LASTFM_API_KEY` absent |
| S4 | Explore | Recommendation | CTE cold start — behaviour when no bliss vectors exist yet |
| S5 | Explore | AnalysisWorker | `analysis_attempts` — no maximum retry cap defined |

---

## Critical Fixes

### C1 — `aubio-sys` C FFI backend used instead of Symphonia

**File:** `crates/adapters-analysis/Cargo.toml`

**Problem.** The walkthrough states:

> "The macOS builds dynamically generate bindings for `aubio-sys`
> (the underlying signal processor for `bliss`) via the `bindgen`
> feature configured inside `adapters-analysis`."

This means the agent used bliss-audio's **default features**, which
pull in `aubio-sys` — a C library binding that requires `aubio`,
`llvm`, and `clang` as system dependencies. This directly contradicts
the locked decision from QB1 (Symphonia: pure Rust, no system deps).

The consequences are significant:
- Linux server deployment now requires `apt install libaubio-dev llvm clang`
- Cross-compilation is broken without a matching sysroot
- CI builds fail on runners without the C toolchain
- The entire point of choosing Symphonia (zero system deps) is lost

**Fix.** In `adapters-analysis/Cargo.toml`, the bliss-audio dependency
must explicitly disable default features and enable only Symphonia:

```toml
[dependencies]
# WRONG — uses aubio/FFmpeg C backend:
bliss-audio = { workspace = true }

# CORRECT — pure Rust Symphonia decoder:
bliss-audio = { workspace = true, default-features = false, features = ["symphonia"] }
```

And in the workspace root `Cargo.toml`:
```toml
[workspace.dependencies]
# Must also set default-features = false here, or the crate-level
# override has no effect if workspace default is also wrong.
bliss-audio = { version = "0.11", default-features = false }
```

After fixing, run:
```bash
cargo build -p adapters-analysis
```

The build output must not reference `aubio-sys`, `bindgen`, or any
C compilation step. The only compilation should be pure Rust.

If bliss-audio 0.11's Symphonia feature does not compile (missing
codec support for a format in the library), document the specific
failure and evaluate whether a small FFmpeg dep is acceptable as a
fallback. Do not silently accept aubio-sys.

---

### C2 — `vector(23)` — compile-time dimension assertion missing

**Files:** `migrations/0002_core_tables-2.sql`, `crates/adapters-analysis/src/lib.rs`

**Problem.** The walkthrough reports `vector(23)`, not `vector(20)` as
the design spec stated. This is plausible — bliss-audio v2 may have
23 features rather than 20. However, the design spec required a
compile-time assertion to catch exactly this discrepancy:

```rust
const _: () = assert!(
    bliss_audio::FEATURES_SIZE == 20,
    "bliss FEATURES_SIZE has changed — update vector(N) in migration"
);
```

The walkthrough does not mention this assertion. Without it, two
scenarios are dangerous:
1. If `FEATURES_SIZE == 23` and `vector(23)` is used: correct, but
   only by accident — a future bliss upgrade could silently change
   the dimension and corrupt stored vectors.
2. If `FEATURES_SIZE != 23` and `vector(23)` was picked by the agent
   without checking: vectors stored in the DB are wrong length and
   every `<->` query returns an error or garbage.

**Fix.** Open `adapters-analysis/src/lib.rs` and verify the value of
`bliss_audio::FEATURES_SIZE`. Then add the assertion:

```rust
// Place at the top of lib.rs, after imports:
const EXPECTED_BLISS_DIMS: usize = bliss_audio::FEATURES_SIZE;

// Compile-time assertion — if FEATURES_SIZE changes, this fails at build:
const _BLISS_DIM_CHECK: () = assert!(
    EXPECTED_BLISS_DIMS == EXPECTED_BLISS_DIMS,  // tautology for now
    // Replace with actual expected value once verified:
    // assert!(EXPECTED_BLISS_DIMS == 23, "...")
);

// Export the constant so the migration value can be verified:
pub const BLISS_VECTOR_DIMS: usize = bliss_audio::FEATURES_SIZE;
```

The key action: **confirm `FEATURES_SIZE` at compile time and ensure
the migration's `vector(N)` matches it exactly.** If `FEATURES_SIZE`
is 23 and the migration already says `vector(23)`, the fix is just
adding the assertion so future bliss upgrades are caught.

Additionally, in `recommendation_repository.rs`, any SQL that casts
a vector literal should reference this constant, not a hardcoded number:

```rust
// When passing a bliss vector as a query parameter, use:
// pgvector's sqlx integration accepts Vec<f32> — the SQL type
// is inferred from the column. No hardcoded dimension needed in Rust.
// But document that the Vec<f32> must have exactly BLISS_VECTOR_DIMS elements.
```

---

### C3 — Stuck `analysis_locked = true` rows after crash

**Files:** `crates/application/src/analysis_worker.rs`, `apps/bot/src/main.rs`

**Problem.** The analysis worker sets `analysis_locked = true` before
analysing a track and clears it on success or failure. If the bot
crashes between the lock acquisition and the result write, the row
is permanently stuck: `analysis_locked = true` with `analysis_status`
still `'processing'`. The analysis worker's poll query filters
`WHERE analysis_locked = false`, so these rows are never retried.

This mirrors the dangling `listen_events` problem from Pass 3.1 (M1).
A crashed analysis session from a single track leaves it locked forever.

**Fix.** On startup, before spawning the analysis worker, run a
cleanup query to unlock stuck rows:

```rust
// In main.rs, before spawning AnalysisWorker:
let unlocked = track_repository
    .unlock_stale_analysis_rows(Duration::from_secs(3600))
    .await
    .expect("failed to unlock stale analysis rows on startup");

if unlocked > 0 {
    tracing::warn!(
        count     = unlocked,
        operation = "analysis_worker.startup_cleanup",
        "unlocked stale analysis rows from previous session"
    );
}
```

New repository method:

```rust
// In track_repository.rs:
pub async fn unlock_stale_analysis_rows(
    &self,
    older_than: Duration,
) -> Result<u64, AppError> {
    // Returns count of unlocked rows
    sqlx::query!(
        r#"
        UPDATE tracks
        SET analysis_locked = false,
            analysis_status = 'pending'
        WHERE analysis_locked = true
          AND analysis_status = 'processing'
          AND updated_at < NOW() - make_interval(secs => $1)
        "#,
        older_than.as_secs() as f64
    )
    // ...
}
```

This requires an `updated_at` column on `tracks`. If one does not
exist, add it (or use `analyzed_at` as a proxy — a track that has
`analysis_locked = true` but `analyzed_at IS NULL` and was started
over an hour ago is definitively stuck).

---

## Major Fixes

### M1 — Mood detection uses hardcoded bliss vector indices — unverified

**File:** `crates/application/src/analysis_worker.rs` or recommendation layer

**Problem.** The design spec's `mood_weight_for_track` function reads
specific indices from the bliss vector to estimate energy:

```rust
let tempo        = bliss_vector.get(0).copied().unwrap_or(0.0);
let spectral_avg = bliss_vector.iter().skip(2).take(4).sum::<f32>() / 4.0;
```

These indices (`[0]` for tempo, `[2..6]` for spectral) are **guesses**
from the design spec, not verified values. The actual bliss v2 feature
layout defines specific indices for each feature type. Using wrong
indices means mood detection reads garbage values — all tracks get
the same mood weight, defeating the entire mood-aware radio feature.

**Fix.** Look up the bliss-audio v2 source code for the feature vector
layout. The authoritative source is the `AnalysisIndex` enum or
equivalent in `bliss_audio::analysis`. For each used index, replace
the hardcoded number with the named constant:

```rust
// Find the correct constants in bliss_audio::analysis:
// Something like:
use bliss_audio::analysis::AnalysisIndex;

let tempo    = bliss_vector[AnalysisIndex::Tempo as usize];
let energy   = bliss_vector[AnalysisIndex::RmsEnergy as usize];  // if it exists
```

If bliss v2 does not export named index constants, check the README
or the `Analysis` struct documentation for the documented layout and
add a comment in the code with the source reference.

Until the correct indices are confirmed, log a warning that mood
detection is using placeholder indices and default to
`MoodWeight::BALANCED` for all tracks:

```rust
// TODO: verify bliss v2 feature vector indices before enabling
// mood-aware weighting. Defaulting to BALANCED until confirmed.
fn mood_weight_for_track(_bliss_vector: &[f32]) -> MoodWeight {
    MoodWeight::BALANCED
}
```

A clearly documented TODO is better than silently wrong mood weights.

---

### M2 — User centroid recomputed on every recommendation call

**File:** `crates/adapters-persistence/src/repositories/recommendation_repository.rs`

**Problem.** The `recommend()` method must provide a `user_centroid`
vector (the weighted average of the user's liked tracks' bliss vectors)
as a parameter to the scoring CTE. If this centroid is computed inside
`recommend()` by running a separate SQL aggregation on every call, the
recommendation response time doubles for every call. `/play` empty-query
autocomplete calls `recommend()` on every keypress — at 25ms debounce,
this runs multiple times per second.

**Fix.** The user centroid should be computed once per `refresh_affinities`
call (which already runs after each listen/favourite event) and cached.
Two acceptable approaches:

**Option A — Pass it in from the lifecycle worker:**
```rust
// In lifecycle_worker.rs, during AffinityUpdate handling:
let centroid = compute_user_centroid(&user_id, &db).await?;
recommendation_port.refresh_affinities(&user_id, &centroid, 50).await?;

// In recommend() signature — centroid is a parameter, always passed:
async fn recommend(
    &self,
    user_id:       &str,
    seed_track_id: Option<Uuid>,
    seed_vector:   Option<Vec<f32>>,
    user_centroid: Option<Vec<f32>>,  // pre-computed, not recomputed here
    mood_weight:   MoodWeight,
    exclude:       &[Uuid],
    limit:         usize,
) -> Result<Vec<TrackSummary>, AppError>;
```

**Option B — Store the centroid in a `user_acoustic_centroids` table:**
```sql
CREATE TABLE user_acoustic_centroids (
    user_id     TEXT PRIMARY KEY,
    centroid    vector(23),   -- use actual FEATURES_SIZE
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```
Updated by `refresh_affinities`, read by `recommend()` in a single
JOIN. This avoids a parameter in the public API but adds a table.

Option B is preferred for Pass 4.1 to ensure correctness.

Verify which approach the current implementation uses. If it recomputes
the centroid inside `recommend()`, apply Option A.

---

### M3 — Stats period boundaries must be calendar-aligned

**File:** `crates/adapters-persistence/src/repositories/user_library_repository.rs`

**Problem.** `user_genre_stats` and `guild_track_stats` use
`period_start` and `period_end` to define time windows for discovery
queries ("Your top genres this month"). If the period is computed as:

```rust
let period_start = Utc::now() - Duration::days(30);
let period_end   = Utc::now();
```

then "this month" is actually a rolling 30-day window. A listen on
March 31 and a listen on April 1 are in the same period. A listen on
March 1 and March 31 are in different periods (March 1's 30-day window
has expired). The discovery page would show incoherent "monthly" stats.

**Fix.** Period boundaries must be calendar month boundaries:

```rust
use chrono::{Utc, Datelike, NaiveDate};

let now          = Utc::now();
let period_start = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
    .unwrap()
    .and_hms_opt(0, 0, 0)
    .unwrap()
    .and_utc();
let period_end = {
    // First day of next month
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
};
```

This ensures all listens in April 2026 have `period_start = 2026-04-01
00:00:00 UTC` regardless of when in April they happened.

---

### M4 — `ON CONFLICT` stats upsert — verify `EXCLUDED` vs table reference

**File:** `crates/adapters-persistence/src/repositories/user_library_repository.rs`

**Problem.** When a listen completes, the stats tables are updated with:

```sql
INSERT INTO user_genre_stats (user_id, genre, play_count, period_start, period_end)
VALUES ($1, $2, 1, $3, $4)
ON CONFLICT (user_id, genre, period_start)
DO UPDATE SET play_count = ???
```

Two wrong patterns that look correct but aren't:

```sql
-- Wrong A: always resets to 2 (EXCLUDED.play_count = 1, plus 1)
DO UPDATE SET play_count = EXCLUDED.play_count + 1

-- Wrong B: correct but verbose
DO UPDATE SET play_count = user_genre_stats.play_count + 1
```

**Fix.** The correct SQL:
```sql
ON CONFLICT (user_id, genre, period_start)
DO UPDATE SET play_count = user_genre_stats.play_count + 1
```

Note: `EXCLUDED.play_count` is the value in the rejected INSERT row
(always `1`), not the current stored value. Using it in the UPDATE
clause would reset the counter to 2 on every listen after the first.

Verify both `user_genre_stats` and `guild_track_stats` use the table
reference form, not the `EXCLUDED` form.

---

### M5 — `AffinityUpdate` may fire on skipped tracks

**File:** `crates/adapters-discord/src/lifecycle_worker.rs`

**Problem.** `AffinityUpdate` should only fire when `completed = true`
— when a user genuinely listened to a track past the 80% threshold.
Firing it on every track end (including skips) would pollute the
affinity table with weak or negative signals treated as positive ones.
A user who skipped a track after 5 seconds should not have their
affinity profile updated as if they enjoyed it.

**Fix.** In the lifecycle worker, the `TrackEnded` handler already
computes `completed = play_duration_ms / track_duration_ms >= THRESHOLD`.
Only dispatch `AffinityUpdate` if the track was completed:

```rust
// In lifecycle_worker.rs, TrackEnded handler:
user_library_port
    .close_listen_event(&user_id, track_id, play_duration_ms, track_duration_ms)
    .await?;

// Only trigger affinity update on genuine listens:
if play_duration_ms as f32 / track_duration_ms as f32
    >= LISTEN_COMPLETION_THRESHOLD
{
    lifecycle_tx.send(TrackLifecycleEvent::AffinityUpdate {
        user_id: user_id.clone(),
    })?;
}
```

Verify this condition exists. If `AffinityUpdate` is dispatched
unconditionally on every `TrackEnded`, add the completion guard.

---

## Optimizations

### O1 — Analysis worker lock: verify `FOR UPDATE SKIP LOCKED`

**File:** `crates/adapters-persistence/src/repositories/track_repository.rs`

The analysis worker must fetch and lock rows atomically to prevent
two concurrent worker instances (or a rapid restart) from analysing
the same track twice. The correct pattern is:

```sql
UPDATE tracks
SET analysis_locked = true, analysis_status = 'processing'
WHERE id IN (
    SELECT id FROM tracks
    WHERE analysis_locked = false
      AND analysis_status IN ('pending', 'failed')
    ORDER BY analysis_attempts ASC, created_at ASC
    LIMIT $batch_size
    FOR UPDATE SKIP LOCKED
)
RETURNING id, blob_location, title
```

`FOR UPDATE SKIP LOCKED` is critical — without it, a concurrent
UPDATE blocks rather than skipping already-locked rows, serialising
the worker unnecessarily. Verify this clause is present.

If the implementation uses two separate queries (SELECT then UPDATE),
there is a race condition: another worker could update the same rows
between the SELECT and UPDATE. The combined UPDATE...WHERE IN pattern
above is atomic.

---

### O2 — Last.fm per-artist cache check — verify single-query batch

**File:** `crates/application/src/lastfm_worker.rs`

The design spec says "For each artist_mbid: check cache, skip if
present, fetch if not." If `ToLastFm` carries multiple artist MBIDs
(a track can have several artists), and the worker checks each MBID
with a separate `SELECT 1 FROM similar_artists WHERE source_mbid = $1`,
that is N separate DB queries per enrichment event.

**Fix.** Batch the cache check:

```sql
SELECT source_mbid
FROM similar_artists
WHERE source_mbid = ANY($1::text[])
GROUP BY source_mbid
```

Then compute the set difference in Rust:

```rust
let cached: HashSet<String> = already_cached.into_iter().collect();
let to_fetch: Vec<&str> = artist_mbids
    .iter()
    .filter(|mbid| !cached.contains(*mbid))
    .map(String::as_str)
    .collect();
```

One DB query instead of N. This matters during initial library
enrichment when thousands of tracks are processed sequentially.

---

### O3 — `user_track_affinities` pruning: old rows accumulate

**File:** `crates/adapters-persistence/src/repositories/recommendation_repository.rs`

`refresh_affinities` upserts the top-50 tracks into `user_track_affinities`.
Without a matching DELETE of old low-score rows, the table grows
indefinitely. A user who listened to 10,000 tracks over a year has
up to 10,000 rows — most stale and irrelevant.

**Fix.** After the UPSERT, prune rows beyond the top-50 limit:

```sql
DELETE FROM user_track_affinities
WHERE user_id = $1
  AND track_id NOT IN (
      SELECT track_id FROM user_track_affinities
      WHERE user_id = $1
      ORDER BY combined_score DESC
      LIMIT 50
  )
```

Run this in the same transaction as the UPSERT. The table then has at
most 50 rows per user at all times.

---

## Self-Explore Items

### S1 — pgvector host installation: document deployment requirement

**File:** `README.md` or deployment docs (wherever they live)

`CREATE EXTENSION IF NOT EXISTS vector` fails with a hard error if
the `pgvector` extension is not installed on the PostgreSQL host:
```
ERROR: could not open extension control file ".../vector.control":
       No such file or directory
```

This will break `cargo sqlx migrate run` on any host without pgvector.
Verify the following are documented:

- **Self-hosted PostgreSQL:** `apt install postgresql-{ver}-pgvector`
  or equivalent. Version must match the PostgreSQL server version.
- **Docker:** Use `pgvector/pgvector:pg16` instead of `postgres:16`.
- **Managed (Supabase/Neon/RDS):** pgvector is generally available but
  may require enabling in the dashboard first.

Also add a helpful error message in `main.rs` startup if the migration
fails due to a missing extension. A startup health check that runs
`SELECT 1 FROM pg_extension WHERE extname = 'vector'` before attempting
full migration would give a clear "pgvector not installed" message
rather than a cryptic migration failure.

---

### S2 — bliss-audio v2 `analysis` field access: verify API

**File:** `crates/adapters-analysis/src/lib.rs`

The walkthrough mentions "bliss-audio Version2." The bliss-audio v2
API may differ from v1 in how the feature vector is accessed. Verify:

1. The `Song` struct in v2 exposes `analysis: Analysis` (not
   `feature: Vec<f32>` as in some older versions).
2. `Analysis` implements `Deref<Target = [f32]>` or has an
   `.as_slice() -> &[f32]` method.
3. `bliss_audio::FEATURES_SIZE` (or equivalent constant) is accessible
   and matches the migration's `vector(N)`.

Check the docs at `docs.rs/bliss-audio/<actual_version>` and confirm
the adapter code calls the correct field/method. Any version mismatch
here will be a compile error (good) or a runtime panic (bad).

---

### S3 — Last.fm graceful degradation when API key absent

**File:** `crates/application/src/lastfm_worker.rs`, `shared-config`

The design spec requires: if `LASTFM_API_KEY` is `None`, the
`LastFmWorker` must pass every `ToLastFm` event through to `ToLyrics`
without any API calls, and log a one-time startup warning.

Verify:
1. Startup logs: `WARN lastfm_worker: LASTFM_API_KEY not set — Last.fm similarity disabled`
2. `ToLastFm` events are forwarded to `ToLyrics` immediately (not dropped)
3. No HTTP request is made to Last.fm when key is absent
4. The recommendation CTE handles `lastfm_score = 0.0` for all tracks
   gracefully (it should by construction, since `similar_artists` table
   is simply empty)

---

### S4 — Recommendation CTE cold start: no bliss vectors yet

**File:** `recommendation_repository.rs`

On a fresh library with no analysed tracks, every `bliss_vector` is
`NULL`. The scoring CTE's acoustic score branch uses:

```sql
CASE
  WHEN t.bliss_vector IS NOT NULL AND $seed_vector IS NOT NULL
  THEN 1.0 / (1.0 + (t.bliss_vector <-> $seed_vector))
  ELSE 0.0
END AS acoustic_score
```

This correctly returns 0.0 when vectors are absent. Verify:
1. The CTE does not error when ALL candidates have `bliss_vector = NULL`
2. Recommendations still return results (taste + lastfm scores carry
   the ranking until vectors are populated)
3. The `/play` empty-query autocomplete returns 25 tracks even before
   any analysis completes (falls back to globally most-played / random)

Test by temporarily setting all `bliss_vector = NULL` in a dev
database and confirming the recommendation flow still returns results.

---

### S5 — `analysis_attempts` retry cap: no maximum defined

**File:** `crates/application/src/analysis_worker.rs`

The worker increments `analysis_attempts` on failure and retries on
the next poll. There is no defined cap. A track whose file is
permanently corrupted or uses an unsupported codec will be retried
indefinitely, forever showing up in the analysis queue.

Evaluate whether to add a cap:

```rust
// In AnalysisWorker, after incrementing attempts:
if attempts >= MAX_ANALYSIS_ATTEMPTS {
    // Mark permanently failed — stop retrying
    repo.set_analysis_status(id, AnalysisStatus::PermanentlyFailed).await?;
    tracing::error!(
        track_id = %id,
        attempts = attempts,
        "track analysis permanently failed — manual intervention required"
    );
} else {
    repo.mark_analysis_failed(id, attempts + 1).await?;
}
```

Suggested cap: `MAX_ANALYSIS_ATTEMPTS = 5`. Add `'permanently_failed'`
as a new `analysis_status` variant if this cap is implemented. Tracks
at this status are never retried automatically but can be reset via
an admin command or direct DB update.

If a cap is not desired (retry forever), document this explicitly and
explain why it is acceptable.

---

## Verification Checklist

```bash
# 1. Confirm pure Rust build — no C compilation output:
cargo build -p adapters-analysis 2>&1 | grep -E "Compiling|running|aubio|bindgen"
# Expected: only Rust crate compilations, NO "running bindgen" or "compiling aubio"

# 2. Dimension assertion fires correctly:
# Temporarily change the assertion to an incorrect value and confirm build fails:
# const _: () = assert!(bliss_audio::FEATURES_SIZE == 999, "test");
# cargo check -p adapters-analysis → should fail with assertion message
# Revert after testing.

# 3. Stuck row cleanup fires on startup:
# Manually set: UPDATE tracks SET analysis_locked=true, analysis_status='processing'
#               WHERE id = (SELECT id FROM tracks LIMIT 1);
# Start the bot → check logs for: analysis_worker.startup_cleanup count=1
# Check DB: analysis_locked should be false, analysis_status = 'pending'

# 4. Completed-only AffinityUpdate:
# Play a track and skip it at 10% → user_track_affinities should NOT update
# Play a track to 85% → user_track_affinities SHOULD update
# Check: SELECT computed_at FROM user_track_affinities WHERE user_id = '<id>'
#        ORDER BY computed_at DESC LIMIT 1;

# 5. Calendar month period boundaries:
# SELECT period_start, period_end, play_count FROM user_genre_stats
# WHERE user_id = '<id>';
# period_start should be the first of the current calendar month (midnight UTC)

# 6. Stats upsert correctness:
# Play the same genre 3 times in the same month
# SELECT play_count FROM user_genre_stats WHERE user_id='<id>' AND genre='<genre>';
# Expected: 3 (not 2, not 1)

# 7. Recommendation with no vectors:
# UPDATE tracks SET bliss_vector = NULL;  (dev DB only)
# /play (empty query) → should still return 25 autocomplete results
# Restore: UPDATE tracks SET bliss_vector = NULL, analysis_status = 'pending';

# 8. Last.fm disabled gracefully:
# Temporarily unset LASTFM_API_KEY in .env
# Start bot → check logs for one-time LASTFM_API_KEY warning
# Verify enrichment pipeline still completes normally
# Verify /play still returns results (lastfm_score = 0 everywhere)
```
