# TeamTI v4 — Audit Pass 2.2
## Full-Flow Search & Autocomplete Audit

> **Scope.** This is a broader exploratory audit than Pass 2.1. Review the
> entire `/play` search lifecycle end-to-end:
>
> 1. User typing in Discord autocomplete
> 2. Autocomplete classification and routing
> 3. Tantivy query building and scoring
> 4. Query-stability-triggered background YouTube fetch
> 5. `youtube_search_cache` persistence
> 6. Incremental Tantivy indexing / startup rebuild
> 7. Submission-value routing into `/play`
> 8. Pass 1 YouTube playback handoff for selected search results
> 9. Track promotion from search stub → playable track
> 10. Display formatting, ranking ergonomics, and operational performance
>
> **Goal.** Find correctness bugs, indexing discrepancies, race conditions,
> UX mismatches, stale-data problems, and optimization opportunities.
> Agents are explicitly encouraged to refactor if it improves invariants,
> search quality, or long-term maintainability.
>
> **External constraints to respect:**
> - Discord autocomplete returns at most 25 choices. [web:651][web:655]
> - Tantivy documents are only visible after `commit()`, and readers reload
>   on commit depending on policy. [web:656][web:663][web:456]
>
> Run this before starting:
> ```bash
> cargo build --workspace 2>&1 | grep -E "^error|^warning"
> cargo sqlx prepare --workspace
> cargo test --workspace
> ```

---

## Audit Questions

This pass is not only a bug sweep. It is a systems audit. The agent should
answer these architectural questions while reviewing the implementation:

1. **Does the search system still feel instant under load?**
   Autocomplete is the hottest path in the bot. If Pass 2 accidentally
   introduced any blocking DB work, writer contention, or slow formatting,
   the whole UX regresses.

2. **Is Tantivy truly the single source of search truth at request time?**
   The pass intentionally moved toward a Tantivy-unified model. If the
   implementation now merges Tantivy, moka, SQL, and ad-hoc fallback logic
   inconsistently, ranking and dedup drift apart.

3. **Does the user see stable, predictable results as they type?**
   The same query should not oscillate wildly between invocations unless new
   data genuinely arrived. The audit should look for result churn caused by
   racey commits, duplicate fetches, or unstable tie-breaking.

4. **Is the search-result-to-play pipeline lossless?**
   If a user selects a `youtube_search` result, the video should flow cleanly
   into Pass 1 playback and end up associated with the eventual `tracks` row.
   No orphaned search stubs, no duplicate track creation, no broken queue item.

5. **Is the foundation solid for later cleanup/ranking passes?**
   Cleanup is intentionally deferred. The agent should still judge whether the
   present design accumulates technical debt too quickly and refactor if needed.

---

## Findings Index

| ID | Severity | Area | Title |
|----|----------|------|-------|
| C1 | Critical | Autocomplete hot path | Blocking work (yt-dlp / DB / Tantivy writer / heavy formatting) in request path |
| C2 | Critical | Search truth model | Tantivy, moka, and DB results can disagree, producing nondeterministic choices |
| C3 | Critical | Dedup | Same video surfaces twice across `tracks` and `youtube_search_cache` |
| C4 | Critical | Promotion path | Selecting a `youtube_search` result can create duplicate tracks or fail to link `track_id` back |
| C5 | Critical | Index consistency | Startup rebuild and incremental updates use different document mapping or filtering rules |
| C6 | Critical | Query stability state | Per-user repeated-query detection is incorrect, leaks state, or triggers fetch storms |
| M1 | Major | Ranking | Playable tracks and transient `youtube_search` stubs are ranked poorly relative to user expectations |
| M2 | Major | Result churn | Same query returns unstable ordering due to missing deterministic tie-breakers |
| M3 | Major | Display | Autocomplete strings are malformed, truncated badly, or violate the 100-char limit |
| M4 | Major | URL preview | YouTube URL preview path bypasses cache sources or returns stale / low-quality info |
| M5 | Major | `yt:` mode | Explicit YouTube mode is not isolated cleanly from standard mode |
| M6 | Major | Tantivy writes | Commit strategy is too eager or too sparse, hurting freshness or responsiveness |
| M7 | Major | Search cache persistence | `youtube_search_cache` upsert semantics lose useful metadata or fail to refresh `last_seen_at` |
| M8 | Major | Submission routing | Raw values from autocomplete are ambiguous or not future-proof |
| O1 | Optimization | Search architecture | Too much request-time merging remains after the Tantivy-unified shift |
| O2 | Optimization | Ranking model | Source-aware boosts/penalties and recency can improve quality without changing UX |
| O3 | Optimization | Writer lifecycle | Reader reload / commit cadence can be improved for lower churn |
| O4 | Optimization | Cache topology | moka + DB + Tantivy responsibilities can be simplified |
| S1 | Explore | Search pipeline | Should `youtube_search_cache` index through the same document builder trait as `tracks`? |
| S2 | Explore | Query parser | Edge-case queries (emoji, punctuation, CJK, quotes, colon prefixes) may behave poorly in Tantivy |
| S3 | Explore | Multi-user contention | Concurrent users searching the same or adjacent queries may race in subtle ways |
| S4 | Explore | Long-term state | Deferred cleanup may already be distorting ranking; detect early warning signs |

---

## Critical Audits

### C1 — Nothing expensive may happen inside autocomplete

**Files:** `crates/adapters-discord/src/...autocomplete...`,
`crates/application/src/youtube_search_worker.rs`,
`crates/adapters-search/src/...`

**Problem to look for.** The autocomplete handler must be effectively
constant-time from the user's perspective. Discord only allows up to 25
choices and the interaction window is tight. [web:651][web:655] Any of the
following in the request path is a critical regression:

- spawning or awaiting yt-dlp directly
- SQL round-trips that could have been pre-indexed into Tantivy
- waiting on the Tantivy writer mutex
- formatting logic that scans DB or filesystem metadata
- full index reloads or synchronous commits

**Required fix if found.** Move all YouTube search work to detached tasks,
keep autocomplete read-only, and ensure result building only touches in-memory
structures plus a Tantivy searcher snapshot.

**Verification:**
```bash
grep -R "yt-dlp\|tokio::process::Command\|search_top_n\|commit()\|IndexWriter" \
  crates/adapters-discord/src crates/application/src | cat
# Manually verify none of these are awaited in the autocomplete handler.
```

---

### C2 — Tantivy, moka, and DB must not disagree at request time

**Problem to look for.** Pass 2 introduced three places where YouTube search
results can exist:

1. moka in-memory fast path
2. `youtube_search_cache` in PostgreSQL
3. Tantivy index

If autocomplete reads more than one of these directly and merges them, it
creates subtle inconsistencies:
- duplicates from the same video in two layers
- different ordering depending on commit timing
- showing results from moka that aren't yet in Tantivy, then losing them on
  next invocation because the search path switched sources

**Target invariant.** At request time, **Tantivy should be the single search
source**, with moka used only as a short-lived write-through acceleration layer
if absolutely necessary. If the current implementation reads moka and Tantivy
both in autocomplete, explore simplifying it. A refactor is allowed if it
makes the search model easier to reason about.

**Preferred fix direction:**
- Standard mode: query Tantivy only
- moka: only suppress duplicate background fetches and optionally hold very
  recent results until Tantivy commit completes; not a parallel ranking source

If you keep moka as a visible result source, document the exact precedence and
ensure dedup + ordering are deterministic.

---

### C3 — Cross-source duplicate results

**Problem.** A video can exist in multiple representations:
- `youtube_search_cache` row
- `tracks` row with `source='youtube'`
- local track that later matches the same YouTube metadata

The same `video_id` must appear at most once in autocomplete. The spec says
`tracks` wins over `youtube_search_cache` during indexing. [code_file:649]

**Audit tasks:**
- Verify startup rebuild applies this rule
- Verify incremental updates apply the same rule
- Verify YouTube URL preview follows the same priority order
- Verify a background fetch cannot temporarily reinsert a search-stub doc for
  a video that already exists in `tracks`

**Fix if broken:** centralize the dedup rule in one function used by both
startup rebuild and incremental update paths.

---

### C4 — Search result selection must promote cleanly into playback

**Problem.** Selecting a `youtube_search` autocomplete result should go through:

`autocomplete choice` → `value` is canonical YouTube URL → `/play` submission
handler → Pass 1 YouTube flow → stub/download job → eventual `tracks` row →
`youtube_search_cache.track_id` linkage

Common failure modes:
- autocomplete returns the wrong `value` type
- `/play` submission parses it as raw text instead of URL
- duplicate track row created because the search stub wasn't checked first
- `track_id` on `youtube_search_cache` never gets linked after download

**Required audit:** Walk this path end-to-end for single result, repeated plays,
and the same video selected from two different queries.

**Fix if broken:** Introduce a single promotion helper:

```rust
async fn promote_search_result_to_track(
    video_id: &str,
    canonical_url: &str,
) -> Result<Uuid, AppError>
```

This helper should:
1. check `tracks.youtube_video_id`
2. if exists, return its `track_id`
3. else create / reuse the Pass 1 stub
4. update `youtube_search_cache.track_id`
5. return the definitive `track_id`

---

### C5 — Startup rebuild and incremental indexing must be identical in semantics

**Problem.** It is common to implement startup rebuild from SQL and later bolt
incremental updates on top with slightly different field mapping. Over time,
this causes a query to return one shape immediately after a live update, then a
different shape after restart.

**Audit tasks:** compare startup indexing vs incremental indexing for:
- field set
- source tagging (`local`, `youtube`, `youtube_search`)
- artist/uploader fallback
- duration handling
- dedup / skip logic
- doc id / deletion term

**Fix if broken:** Extract a shared document mapper:

```rust
trait ToSearchDoc {
    fn to_search_doc(&self) -> SearchDoc;
}
```

Use the same mapper in rebuild and incremental writes.

---

### C6 — Query stability detection must be race-safe and bounded

**Problem.** The repeated-query model is simple conceptually, but buggy state
management can cause:
- a query never fetching because `last_query` is overwritten too early
- duplicate fetch storms because `pending_fetches` isn't checked atomically
- per-user state leaking forever
- one user's repeated query unblocking another user's background fetch

**Audit tasks:**
- inspect `DashMap<UserId, UserAutocompleteState>` usage
- verify normalization is applied before comparison
- verify `pending_fetches` is cleaned on both success and failure
- verify a crash in `fetch_and_cache` cannot leave a query permanently pending

**Preferred refactor if needed:** replace ad-hoc `last_query + HashSet` with a
small state machine struct per user:

```rust
struct QueryProbe {
    last_query: String,
    pending: HashSet<String>,
    last_seen_at: Instant,
}
```

and add a periodic sweep of stale users.

---

## Major Audits

### M1 — Ranking should feel human, not just technically correct

Audit whether autocomplete currently over-surfaces transient `youtube_search`
stubs when the user would obviously prefer a playable local or cached YouTube
track. If two results match equally, playable content should probably outrank
search stubs.

Explore source-aware scoring such as:

```rust
final_score = tantivy_score
            + source_boost(source)
            + recency_boost(last_seen_or_last_played)
            + play_count_boost(play_count)
```

Suggested source boosts to experiment with:
- local: +0.20
- youtube (downloaded/playable): +0.15
- youtube_search: +0.00

Only apply if it improves ranking empirically. If the existing ranking is
already excellent, document that and keep it simple.

---

### M2 — Deterministic tie-breaking

If multiple results have the same or nearly identical score, the ordering must
be deterministic or users will perceive autocomplete as jittery. Add explicit
secondary keys if missing, e.g.:

1. higher score
2. source priority (`local` > `youtube` > `youtube_search`)
3. recent play / recent seen
4. shorter title
5. stable id order

Audit whether current ordering changes between invocations for the same query
with no new data.

---

### M3 — Formatting edge cases

Audit the rendering layer for:
- title-only results with no broken `—` or `·`
- missing duration (`0` or NULL) should render consistently (`--:--` or omit)
- Unicode / emoji length counting vs Discord 100-char limit
- overlong uploader names on YouTube results
- CJK titles where naive char truncation may still feel awkward

Discord autocomplete has a hard 25-choice limit and practical display limits.
[web:651][web:655] Ensure choices are valid and ergonomic.

---

### M4 — URL preview quality and staleness

Audit the precedence order for URL preview:
1. `tracks` by `youtube_video_id`
2. `youtube_search_cache` by `video_id`
3. fallback placeholder

Verify the implementation does not accidentally prefer lower-quality search
cache metadata over richer track metadata, and that the preview uses the same
artist fallback rules as standard results.

Explore whether a lightweight background refresh of URL preview metadata is
worth it when a URL is known only from stale search cache. If too complex,
document and defer.

---

### M5 — `yt:` mode must be a true mode

Audit whether `yt:` actually changes behavior everywhere it should:
- autocomplete classification
- Tantivy filter
- background fetch trigger (immediate, not stability-based)
- submission handling if raw `yt:` is submitted without selecting a choice
- ranking rules (local results must not leak in)

If any layer partially ignores `yt:`, fix it.

---

### M6 — Commit strategy may be wrong

Tantivy only exposes writes after `commit()`. [web:656][web:663][web:456]
Audit whether commits happen:
- once per result row (too expensive)
- once per query batch (reasonable)
- too infrequently (results feel stale)

A good default is one commit per background fetch batch, not per row.
If the implementation commits excessively, refactor to stage documents then
commit once. If it commits too rarely, users won't see results soon enough.

---

### M7 — `youtube_search_cache` upsert semantics

Audit the SQL carefully. On conflict by `video_id`, does the upsert:
- refresh `last_seen_at`
- preserve richer existing metadata if new result is poorer
- update query text sensibly or leave first-seen query intact
- avoid clearing `track_id`

Because cleanup is deferred, these semantics matter. A weak upsert can slowly
degrade metadata quality over time.

Suggested policy:

```sql
ON CONFLICT (video_id) DO UPDATE
SET
    last_seen_at = now(),
    title        = COALESCE(EXCLUDED.title, youtube_search_cache.title),
    uploader     = COALESCE(EXCLUDED.uploader, youtube_search_cache.uploader),
    channel_id   = COALESCE(EXCLUDED.channel_id, youtube_search_cache.channel_id),
    duration_ms  = COALESCE(EXCLUDED.duration_ms, youtube_search_cache.duration_ms),
    thumbnail_url= COALESCE(EXCLUDED.thumbnail_url, youtube_search_cache.thumbnail_url),
    track_id     = COALESCE(youtube_search_cache.track_id, EXCLUDED.track_id)
```

Review whether storing only one `query` is still sensible once a video is seen
under many searches. If not, document for a later pass.

---

### M8 — Submission values must be stable and future-proof

Audit whether autocomplete values are too clever or ambiguous. If values can be
UUIDs, URLs, and raw strings, ensure they cannot collide accidentally.

Explore whether an explicit encoded envelope would be safer:

```text
track:UUID
yturl:https://www.youtube.com/watch?v=...
raw:query text
```

If the current implementation is brittle, refactor now. A stable value protocol
will pay off in later passes.

---

## Optimization Opportunities

### O1 — Reduce request-time merging

If autocomplete still merges multiple ranked lists in complicated ways, explore
moving more logic into the indexed documents themselves. For example, include a
`source_priority` numeric field or a `playable` boolean in Tantivy docs so the
search layer can rank closer to final UX before formatting.

---

### O2 — Source-aware ranking refinement

Test whether mild source boosts and recency improve results materially. If yes,
implement them and add tests. If no, document that the current weighting is
already sufficient.

---

### O3 — Reader reload / writer lifecycle

Tantivy readers typically operate as long-lived snapshots and refresh on commit
depending on reload policy. [web:663] Audit whether the reader reload strategy
is appropriate for a bot autocomplete workload. If reloads are too frequent or
manual refresh is missing, fix that.

---

### O4 — Cache topology simplification

If moka is only used to bridge a tiny freshness window, consider whether a
small in-flight fetch registry plus Tantivy is enough. Conversely, if moka is
serving real UX value, make that role explicit. The agent should decide after
measuring complexity vs benefit.

---

## Self-Explore Areas

### S1 — Unify document builders

Investigate whether `tracks` and `youtube_search_cache` documents are built by
duplicate code. If so, refactor into a shared `SearchDoc` builder and test it.
This reduces drift between startup rebuild and live updates.

---

### S2 — Query parser edge cases

Run exploratory tests for:
- punctuation-heavy queries (`!!!`, `-`, `/`, `:`)
- emoji-only queries
- quoted phrases
- CJK input
- mixed Latin + emoji
- `yt:` with extra spaces (`yt:  query`)
- URL-like strings that are not valid URLs

If Tantivy parsing or classification behaves poorly, harden normalization and
fallback behavior.

---

### S3 — Multi-user race testing

Simulate several users typing:
- the same query concurrently
- slightly different prefixes of the same query
- alternating `yt:` and standard searches

Verify there are no:
- duplicate fetches for same normalized query
- inconsistent result ordering
- broken per-user stability state
- accidental cross-user coupling

---

### S4 — Long-term stale-state effects

Because cleanup is deferred, examine whether ranking already degrades after the
search cache grows. Generate many fake search cache rows and inspect whether
rarely-seen stale rows pollute top results. If yes, consider adding a mild
recency decay now rather than waiting for cleanup.

---

## Verification Checklist

```bash
# Build and schema:
cargo sqlx migrate run
psql -c "\d youtube_search_cache"
cargo sqlx prepare --workspace
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# C1 — no expensive work in autocomplete:
grep -R "tokio::process::Command\|search_top_n\|IndexWriter\|commit()" \
  crates/adapters-discord/src crates/application/src
# Verify autocomplete handler itself only reads normalized input + Tantivy/search state.

# C2/C3 — search truth + dedup:
# Search a query whose top result exists both in youtube_search_cache and tracks.
# Expected: appears exactly once.

# C4 — promotion path:
# 1. Search query → select youtube_search result
# 2. Submit /play
# 3. Verify playback starts and later:
#    SELECT track_id FROM youtube_search_cache WHERE video_id='...';
#    → non-null
#    SELECT count(*) FROM tracks WHERE youtube_video_id='...';
#    → exactly 1

# C5 — rebuild vs live update parity:
# 1. Search to create youtube_search_cache rows
# 2. Restart bot (forces startup rebuild)
# 3. Search again
# Expected: same source labels, same dedup, similar ordering

# C6 — repeated query trigger:
# Same user, same query twice → background fetch triggered once
# Several users, same query → no fetch storm

# M1/M2 — ranking stability:
# Repeat same query 10 times with no new data
# Expected: identical top ordering each time

# M3 — formatting:
# Very long title/uploader, null artist, zero duration, emoji titles
# Ensure 100-char-safe and visually clean

# M5 — yt mode:
# /play yt:query → no local results, fetch immediately
# submit raw yt:query (without selecting) → still routes to YouTube search

# M6 — commit batching:
# Search query that returns 5 YouTube results
# Expected: one batch commit, not 5 per-row commits

# S4 — stale-state sampling:
# Seed many youtube_search_cache rows and inspect ranking drift manually
```

---

## Output Requirements

At the end of the audit, produce:

1. **A findings list** grouped by Critical, Major, Optimization, Explore
2. **A short architecture judgment** answering whether the Pass 2 foundation
   is solid enough for future cleanup/ranking passes
3. **Any refactors applied** (especially if you unified document builders,
   stabilized submission values, or simplified the cache topology)
4. **Any deferred concerns** that are not broken now but should be tracked

If the implementation is mostly correct, spend the remaining effort on:
- deterministic ranking
- simplifying search truth flow
- reducing duplicate logic
- stress-testing concurrency and stale-cache behavior
