# TeamTI v3 — Pass 4 Implementation Prompt
## Enhanced Recommendation Engine, Radio & Discovery Foundation

> Attach alongside: `teamti_v3_pass4_design.md` (schema, ports, scoring
> CTE, worker logic, pipeline changes, and all locked decisions).
> Also attach: all Pass 3 output files, `main.rs`, the existing
> pipeline workers (`musicbrainz_worker.rs`, `lyrics_worker.rs`),
> and the existing `lifecycle_worker.rs`.
>
> The design spec is authoritative. This prompt defines goals,
> end-to-end UX, build philosophy, and the definition of done.

---

## What This Pass Builds

Pass 3 gave users a personal layer — playlists, favourites, listen
history, and a baseline recommendation engine that scores tracks
using genre and artist metadata. That engine is honest but limited:
it can only recommend music you've already been exposed to through
genre and artist. It cannot discover new things that *sound* like
what you like.

Pass 4 makes recommendation genuinely useful by adding three layers:

1. **Acoustic analysis (bliss-audio)** — every track in the library
   gets a 20-dimensional feature vector representing how it sounds:
   tempo, energy, spectral texture. Radio mode can now find tracks
   that genuinely sound like the seed, not just share a genre tag.

2. **Social graph (Last.fm)** — at enrichment time, similar artists
   are fetched and cached permanently. The recommendation engine uses
   this as a soft cross-artist signal without any live API calls in
   the hot path.

3. **Materialized affinities** — a user's taste profile (their
   acoustic centroid, genre mix, artist preferences) is pre-computed
   after every significant listening event and stored in
   `user_track_affinities`. The discovery page (future web portal)
   reads directly from this table — no on-demand computation needed.

---

## End Goals — What Must Work When This Pass Is Done

### Analysis Worker

From the moment the bot starts, a background worker polls for tracks
with `analysis_status = 'pending'` or `'failed'` and analyses them
using bliss-audio over Symphonia. Analysis is low-priority — it runs
in a separate thread pool and never blocks enrichment or playback.

After a fresh library scan, the user should be able to watch
`SELECT count(*) FROM tracks WHERE bliss_vector IS NOT NULL` grow
over time as the worker processes the backlog. The default concurrency
is 4 (configurable via `ANALYSIS_CONCURRENCY`).

If the NAS is unreachable (file not found), the track is marked
`failed` with an incremented attempt counter and retried on the next
run. The worker does not crash — it logs a warning and moves on to
the next track.

### Last.fm Enrichment Stage

After MusicBrainz enrichment assigns artist MBIDs, a new `LastFmWorker`
stage fetches `artist.getSimilar` for each artist and caches the results
in `similar_artists`. This runs once per artist — subsequent tracks by
the same artist skip the API call (cache hit). The worker is non-fatal:
a Last.fm failure logs a warning and the next pipeline stage runs anyway.

If `LASTFM_API_KEY` is not set in the environment, the `LastFmWorker`
passes every event through to `ToLyrics` without making any API calls
and logs a one-time startup warning. The rest of the system works
normally — Last.fm similarity scores just remain 0.

### Enhanced Radio

`/radio` now produces recommendations that are acoustically coherent.
A high-energy seed (detected from its bliss vector's tempo and spectral
components) drives `MoodWeight::ACOUSTIC_DOMINANT` — subsequent tracks
should feel similar in energy and texture. A calm, low-energy seed
drives `MoodWeight::TASTE_DOMINANT` — subsequent tracks match the user's
genre and artist preferences more than the exact sound.

The blend is automatic and invisible to the user — no new commands,
no new flags. Radio still refills silently when the queue drops to ≤2
tracks, exactly as in Pass 3. The only observable difference is that
the track selection is better.

### Affinity Updates & Discovery Foundation

After every completed listen and every favourite action, the user's
top-50 recommended tracks are recomputed and written to
`user_track_affinities`. This is the foundation the web portal's
"You might like" section will read from.

Simultaneously, `user_genre_stats` and `guild_track_stats` are updated
for each completed listen. By end of Pass 4, a database query against
these tables should return meaningful "top genres this month" and "most
popular in this server" data — no portal needed to verify this, just
a direct SQL query in the verification step.

---

## Implementation Philosophy

### Verify bliss dimensions before touching the schema

Before writing the migration, add this assertion to `adapters-analysis`:

```rust
const _: () = assert!(
    bliss_audio::FEATURES_SIZE == 20,
    "bliss FEATURES_SIZE changed — update vector(N) in migration"
);
```

Run `cargo check` on `adapters-analysis`. If the assertion fails, the
correct dimension is whatever `FEATURES_SIZE` is — update `vector(20)`
in the migration and all SQL references. Only then run the migration.
This is a one-time check that prevents a silent data corruption
(storing a 20-dim vector in a `vector(16)` column truncates silently).

### pgvector is a PostgreSQL extension, not a database switch

`CREATE EXTENSION IF NOT EXISTS vector` goes at the top of
`0001_extensions.sql`. That is the entire infrastructure change.
No new services, no new containers, no new dependencies beyond the
extension being installed on the PostgreSQL host. If the host is a
managed PostgreSQL service (e.g., Supabase, Neon, RDS), verify that
`pgvector` is available on that tier before proceeding.

### Two new crates, same hexagonal pattern

`adapters-analysis` and `adapters-lastfm` follow the exact same
structure as `adapters-musicbrainz` and `adapters-lrclib`:
- Implement a port trait from `crates/application`
- Accept config from `shared-config`
- Return `AppError` variants, not raw library errors
- No Discord or database knowledge

Do not put any bliss analysis logic inside the application layer or
the persistence adapter. bliss is an I/O+CPU concern — it belongs
in `adapters-analysis` only.

### Workers are application-layer, not adapter-layer

`AnalysisWorker` and `LastFmWorker` live in `crates/application`,
just like `MusicBrainzWorker` and `LyricsWorker` already do. They
take ports by Arc, know nothing about HTTP or file paths, and express
their logic purely in terms of domain types and port methods.

### The recommendation CTE replaces, not wraps, the Pass 3 CTE

Pass 3 has a CTE-based scoring query in `PgRecommendationRepository`.
Pass 4's CTE completely replaces it. Do not layer Pass 4 scoring on
top of the existing query. Read the Pass 3 query first, understand
what it does, then write the new one from scratch using the Pass 4
spec. The interface (`RecommendationPort`) will have a changed
signature — update all call sites.

### Affinity updates are fire-and-forget from the lifecycle worker

When the lifecycle worker sees a completed listen or a favourite event,
it dispatches `TrackLifecycleEvent::AffinityUpdate { user_id }` to the
same unbounded channel. The handler for this event calls
`RecommendationPort::refresh_affinities(user_id, limit: 50)` and
logs the result. A failure in affinity refresh must never bubble up to
the Discord interaction — it is a background maintenance task.

### Analysis worker uses spawn_blocking correctly

bliss analysis is synchronous and CPU-bound. Do not call it directly
from an async context — wrap every `bliss_audio::Song::from_path` call
in `tokio::task::spawn_blocking`. The worker's poll loop is async;
the individual analysis tasks are blocking. Use a `JoinSet` or
`FuturesUnordered` to bound concurrency to `ANALYSIS_CONCURRENCY`.

### Last.fm errors never block the pipeline

The `ToLastFm → ToLyrics` pipeline stage must always emit `ToLyrics`,
even on a Last.fm API failure. Last.fm enrichment is optional — a track
without Last.fm data is fully functional. The only effect of a missing
Last.fm cache entry is that `lastfm_score = 0.0` in the recommendation
CTE, which is acceptable.

---

## What This Pass Does NOT Do

- No Collaborative Filtering — not enough user×track interactions yet.
- No neural networks, no burn, no tch-rs.
- No Qdrant, no external vector database.
- No audio fingerprint similarity (AcoustID is for identification only).
- No web portal HTTP endpoints — Discord-only, same as Pass 3.
- No `/radio acoustic` vs `/radio taste` toggle — mood is auto-detected.
- No user-visible "analysis progress" indicator.
- No re-analysis on tag changes — bliss vectors are immutable once set.
  (A `/rescan` command that clears bliss_vector and re-analyses is
  deferred to Pass 4.1 if needed.)

---

## Definition of Done

Pass 4 is complete when:

1. `cargo sqlx migrate run` applies cleanly on a fresh database.
   `psql -c "\dx" | grep vector` shows the extension installed.
2. `cargo sqlx prepare --workspace` passes with zero errors.
3. `cargo build --workspace` produces zero errors and zero warnings.
4. `cargo test --workspace` passes.
5. `bliss_audio::FEATURES_SIZE` assertion is present in `adapters-analysis`
   and `cargo check` confirms it matches the migration's `vector(N)`.
6. The analysis worker runs from startup. After 60 seconds, at least one
   track has `analysis_status = 'done'` and a non-null `bliss_vector`.
7. After a MusicBrainz enrichment completes for a mainstream artist,
   `SELECT count(*) FROM similar_artists WHERE source_mbid = '<mbid>'`
   returns > 0.
8. `/radio` with a high-energy seed queues tracks that are observably
   different in character from `/radio` with a low-energy seed.
9. After a completed listen: `SELECT * FROM user_track_affinities
   WHERE user_id = '<id>' ORDER BY combined_score DESC LIMIT 5`
   returns rows.
10. `SELECT genre, play_count FROM user_genre_stats WHERE user_id = '<id>'
    ORDER BY play_count DESC LIMIT 5` returns meaningful genre data.
11. `SELECT track_id, play_count FROM guild_track_stats
    WHERE guild_id = '<id>' ORDER BY play_count DESC LIMIT 10`
    returns the server's most-played tracks.
