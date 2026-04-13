# TeamTI v4 — Pass 2 Design Spec
## Unified Search: Tantivy-Backed Autocomplete with YouTube Integration

---

## Decision Log

| Topic | Decision |
|-------|----------|
| YouTube search results storage | Separate `youtube_search_cache` table — `tracks` stays playable-media-only |
| Search backend | Tantivy unified — indexes both `tracks` and `youtube_search_cache` |
| In-session fast path | `moka` async LRU cache (write-through to Tantivy) |
| YouTube autocomplete trigger | Query stability detection (same query fires twice consecutively per user) |
| Result merging | Always include all local results; YouTube fills remainder heuristically |
| Display format | `🎵/📺 Title — Artist · M:SS` (Option D) |
| Artist fallback (YouTube) | `artist_display` → `youtube_uploader` → title only |
| URL input | Resolved preview: show title if cached, `▶ YouTube: {video_id}` if not |
| Unsupported URLs | Silently empty autocomplete; error returned on submission |
| Explicit YouTube search | `yt:` prefix bypasses local library entirely |
| Cache scope | Global (shared across all guilds — single NAS/IP) |
| Cache TTL | 15 minutes, LRU cap 500 queries |
| DB cleanup | Deferred to a later pass |

---

## Architecture Overview

```
User types in /play autocomplete
          │
          ├─ classify_input(query)
          │
          ├─ YoutubeUrl ──► resolved_preview(video_id)
          │                  cache HIT  → "▶ Title — Artist · M:SS"
          │                  cache MISS → "▶ YouTube: {video_id}"
          │
          ├─ YoutubeOnly ("yt:" prefix)
          │     └─► tantivy.search(stripped, filter=youtube_only)
          │             + always trigger background fetch
          │             + return all YouTube results (up to 25)
          │
          └─ Standard (plain text)
                └─► tantivy.search(query, all_sources)   // <1ms
                        │
                        ├─ merge_heuristic(local, yt_from_tantivy)
                        │
                        └─ check query stability:
                             same query twice in a row for this user?
                               AND local_count < 20?
                             → spawn background_fetch(query)

background_fetch(query):
  yt-dlp "ytsearch5:{query}"           // ~2-4s, runs async
  → insert into youtube_search_cache   // persistence
  → update moka cache                  // in-session fast path
  → commit tantivy index               // future queries find results
```

---

## New Table: `youtube_search_cache`

Migration `0008_youtube_search_cache.sql`:

```sql
-- Transient YouTube search result stubs.
-- These are NOT playable tracks — they are metadata previews from
-- YouTube search results used to enrich autocomplete suggestions.
-- Cleanup is deferred to a later pass; rows accumulate safely.
--
-- Separation rationale: tracks = confirmed playable local media only.
-- youtube_search_cache = ephemeral search stubs that may never be played.
CREATE TABLE IF NOT EXISTS youtube_search_cache (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    video_id      TEXT NOT NULL,
    title         TEXT NOT NULL,
    uploader      TEXT,
    channel_id    TEXT,
    duration_ms   INTEGER,
    thumbnail_url TEXT,
    -- Normalised search query that surfaced this result.
    -- A video may appear under multiple queries; only the first is stored.
    query         TEXT NOT NULL,
    -- If this video was subsequently queued/downloaded, link to the tracks row.
    -- ON DELETE SET NULL: the search stub outlives its associated track row.
    track_id      UUID REFERENCES tracks(id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_youtube_search_cache_video_id
    ON youtube_search_cache(video_id);

CREATE INDEX IF NOT EXISTS idx_youtube_search_cache_query
    ON youtube_search_cache(query text_pattern_ops);

CREATE INDEX IF NOT EXISTS idx_youtube_search_cache_last_seen
    ON youtube_search_cache(last_seen_at DESC);
```

---

## Tantivy Schema Extension

The existing Tantivy index (from v3) indexes `tracks`. Pass 2 extends
it to also index `youtube_search_cache` rows.

### New fields added to the Tantivy schema

```rust
// Added to the existing schema builder:
schema_builder.add_text_field("source",           STRING | STORED);  // "local" | "youtube" | "youtube_search"
schema_builder.add_text_field("youtube_video_id", STRING | STORED);  // for dedup + URL reconstruction
schema_builder.add_text_field("uploader",         TEXT   | STORED);  // YouTube channel name
schema_builder.add_u64_field ("duration_ms",      STORED | FAST);    // for display
```

### Index population sources

Two index rebuild sources (run at startup and on incremental updates):

```rust
// Source 1: tracks table (existing)
// New: source = tracks.source ("local" or "youtube")
// New: youtube_video_id = tracks.youtube_video_id (may be NULL → omit)

// Source 2: youtube_search_cache table (new)
// source = "youtube_search"
// youtube_video_id = youtube_search_cache.video_id
// title = youtube_search_cache.title
// artist_display = youtube_search_cache.uploader (reuse artist_display field)
// duration_ms = youtube_search_cache.duration_ms
```

### Deduplication in the index

A video_id may exist in BOTH `tracks` (if downloaded) and
`youtube_search_cache`. The Tantivy index must not return duplicates.
Deduplication strategy: **prefer the `tracks` document over the
`youtube_search_cache` document for the same video_id.** When building
the index, skip any `youtube_search_cache` row whose `video_id` already
has a corresponding entry in `tracks`.

On incremental update (when a download completes and a stub is promoted
from YouTube stub to a tracks row with `blob_location`): delete the
`youtube_search_cache` Tantivy document by video_id, add/update the
`tracks` Tantivy document.

```rust
// In YoutubeDownloadWorker, after complete_download_job():
tantivy_writer.delete_by_term(Term::from_field_text(video_id_field, &video_id));
tantivy_writer.add_document(track_to_tantivy_doc(&track));
tantivy_writer.commit().await?;
```

---

## Autocomplete Input Classification

```rust
#[derive(Debug, PartialEq)]
pub enum AutocompleteMode<'a> {
    /// Plain text search — query local + YouTube via heuristic
    Standard(&'a str),
    /// "yt:" prefix — bypass local, force YouTube-only results
    YoutubeOnly(&'a str),
    /// Recognised YouTube URL — show resolved preview
    YoutubeUrl { video_id: String },
    /// Non-YouTube URL — silently return empty; error on submit
    UnsupportedUrl,
}

pub fn classify_autocomplete_input(input: &str) -> AutocompleteMode {
    let trimmed = input.trim();

    if let Some(query) = trimmed.strip_prefix("yt:") {
        return AutocompleteMode::YoutubeOnly(query.trim());
    }
    if let Some(video_id) = extract_youtube_video_id(trimmed) {
        return AutocompleteMode::YoutubeUrl { video_id };
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return AutocompleteMode::UnsupportedUrl;
    }
    AutocompleteMode::Standard(trimmed)
}
```

---

## Result Merging Heuristic

```rust
/// Maximum YouTube results to include based on local result count.
/// Local results are always included in full; YouTube fills the remainder.
fn youtube_budget(local_count: usize) -> usize {
    match local_count {
        0        => 25,
        1..=4    => 20,
        5..=9    => 10,
        10..=19  => 5,
        _        => 0,
    }
}

/// Merge local and YouTube results into at most 25 autocomplete choices.
/// Invariant: ALL local results are always included, never truncated.
pub fn merge_results(
    local: Vec<SearchResult>,
    youtube: Vec<SearchResult>,
) -> Vec<AutocompleteChoice> {
    let budget = youtube_budget(local.len());
    let yt_take = budget.min(youtube.len());

    local.iter()
        .chain(youtube.iter().take(yt_take))
        .map(format_choice)
        .collect()
}
```

---

## Display Format

All autocomplete choices use Option D:
`{icon} {title} — {artist} · {duration}`

```rust
fn format_choice(result: &SearchResult) -> AutocompleteChoice {
    let icon = match result.source.as_str() {
        "local"          => "🎵",
        "youtube"        => "📺",
        "youtube_search" => "📺",
        _                => "🎵",
    };

    let artist = effective_artist(result);
    let duration = format_duration(result.duration_ms);

    // Build display string, respecting Discord's 100-char limit on name
    let display = if let Some(ref a) = artist {
        format!("{icon} {} — {} · {}", result.title, a, duration)
    } else {
        format!("{icon} {} · {}", result.title, duration)
    };

    // Truncate to 100 chars, appending "…" if needed
    let name = if display.chars().count() > 100 {
        format!("{}…", display.chars().take(99).collect::<String>())
    } else {
        display
    };

    // The value submitted to the command handler:
    // For local/youtube tracks: track UUID (for instant local lookup)
    // For youtube_search stubs: canonical YouTube URL
    let value = match result.source.as_str() {
        "youtube_search" => canonical_youtube_url(&result.youtube_video_id.as_deref().unwrap()),
        _                => result.track_id.to_string(),
    };

    AutocompleteChoice::new(name, value)
}

/// Artist resolution order:
/// 1. artist_display (enriched — best quality)
/// 2. youtube_uploader (for YouTube tracks/stubs before enrichment)
/// 3. None → title-only display
fn effective_artist(result: &SearchResult) -> Option<String> {
    result.artist_display.clone()
        .or_else(|| result.uploader.clone())
}
```

---

## URL Resolved Preview

When the user pastes a YouTube URL, the autocomplete shows a resolved
preview rather than the raw URL.

```rust
AutocompleteMode::YoutubeUrl { video_id } => {
    // Check tracks table first (downloaded/stub)
    let preview = if let Some(track) = repo.find_by_youtube_video_id(&video_id).await? {
        let artist = effective_artist_from_track(&track);
        let dur = format_duration(track.duration_ms.unwrap_or(0));
        if let Some(a) = artist {
            format!("▶ {} — {} · {}", track.title, a, dur)
        } else {
            format!("▶ {}", track.title)
        }
    }
    // Then check youtube_search_cache (search stub)
    else if let Some(stub) = repo.find_search_cache_by_video_id(&video_id).await? {
        let dur = format_duration(stub.duration_ms.unwrap_or(0));
        if let Some(ref u) = stub.uploader {
            format!("▶ {} — {} · {}", stub.title, u, dur)
        } else {
            format!("▶ {}", stub.title)
        }
    }
    // Unknown video — show placeholder
    else {
        format!("▶ YouTube: {}", &video_id)
    };

    // Truncate to 100 chars
    let name = truncate_display(&preview, 100);
    // Value is always the canonical URL — Play handler resolves it
    let value = canonical_youtube_url(&video_id);
    vec![AutocompleteChoice::new(name, value)]
}
```

---

## Query Stability Detection + Background Fetch

### State

```rust
/// Per-user autocomplete state — stored in a DashMap keyed by UserId.
#[derive(Default)]
pub struct UserAutocompleteState {
    /// Last query string seen for this user.
    pub last_query: String,
    /// Queries currently being fetched in the background.
    /// Prevents duplicate concurrent fetches for the same query.
    pub pending_fetches: HashSet<String>,
}
```

### Autocomplete handler

```rust
pub async fn handle_autocomplete(
    ctx:    &Context,
    user_id: UserId,
    raw_input: &str,
    search_state: &SearchState,   // holds DashMap, moka cache, Tantivy reader
) -> Vec<AutocompleteChoice> {

    let input = raw_input.trim().to_lowercase();

    match classify_autocomplete_input(&input) {
        AutocompleteMode::Standard(query) => {
            // 1. Always query Tantivy (local + cached YouTube results, <1ms)
            let all_results = search_state.tantivy.search(query, SearchFilter::All, 25).await;

            let (local, youtube): (Vec<_>, Vec<_>) = all_results.into_iter()
                .partition(|r| r.source == "local");

            // 2. Trigger background fetch if query is stable and local is sparse
            let should_fetch = {
                let mut state = search_state.user_state.entry(user_id).or_default();
                let is_stable = state.last_query == query;
                let not_pending = !state.pending_fetches.contains(query);
                let is_sparse = local.len() < 20;
                state.last_query = query.to_string();
                is_stable && not_pending && is_sparse && query.len() >= 2
            };

            if should_fetch {
                search_state.user_state
                    .entry(user_id)
                    .or_default()
                    .pending_fetches
                    .insert(query.to_string());

                let worker = search_state.yt_search_worker.clone();
                let q = query.to_string();
                tokio::spawn(async move {
                    worker.fetch_and_cache(q).await;
                });
            }

            merge_results(local, youtube)
                .into_iter()
                .map(format_choice)
                .collect()
        }

        AutocompleteMode::YoutubeOnly(query) => {
            // Always trigger background fetch for explicit yt: searches
            let results = search_state.tantivy
                .search(query, SearchFilter::YoutubeOnly, 25)
                .await;

            // Unconditionally trigger background fetch
            if query.len() >= 2 {
                let worker = search_state.yt_search_worker.clone();
                let q = query.to_string();
                tokio::spawn(async move { worker.fetch_and_cache(q).await; });
            }

            results.into_iter().map(format_choice).collect()
        }

        AutocompleteMode::YoutubeUrl { video_id } => {
            handle_url_preview(&video_id, search_state).await
        }

        AutocompleteMode::UnsupportedUrl => vec![],
    }
}
```

Note: `pending_fetches` entries are cleaned up after `fetch_and_cache`
completes. The `DashMap<UserId, UserAutocompleteState>` entries for
users who haven't interacted recently can be cleaned periodically
(e.g., via moka's TTL on a wrapper, or a simple periodic sweep).

---

## Background Search Worker: `YoutubeSearchWorker`

New struct in `crates/application/src/youtube_search_worker.rs`.
Distinct from `YoutubeDownloadWorker` — lighter, no Semaphore needed
(search calls are fast and don't compete for download bandwidth).

```rust
pub struct YoutubeSearchWorker {
    ytdlp:          Arc<dyn YtDlpPort>,
    repo:           Arc<dyn YoutubeSearchRepository>,
    tantivy_writer: Arc<TantivyWriter>,
    moka_cache:     Arc<moka::future::Cache<String, Vec<YoutubeSearchResult>>>,
}

impl YoutubeSearchWorker {
    pub async fn fetch_and_cache(&self, query: String) {
        // Guard: check moka first — may have been populated by a concurrent fetch
        if self.moka_cache.contains_key(&query) {
            return;
        }

        let results = match self.ytdlp.search_top_n(&query, 5).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(query, error = %e, "youtube search failed");
                return;
            }
        };

        // 1. Persist to youtube_search_cache (upsert — video_id is the conflict key)
        for result in &results {
            let _ = self.repo.upsert_search_result(&query, result).await;
        }

        // 2. Add to Tantivy (skip any video_id already in tracks)
        let existing_video_ids = self.repo
            .find_existing_video_ids(
                &results.iter()
                    .filter_map(|r| r.video_id.as_deref())
                    .collect::<Vec<_>>()
            ).await.unwrap_or_default();

        for result in &results {
            if let Some(ref vid) = result.video_id {
                if !existing_video_ids.contains(vid.as_str()) {
                    let doc = search_result_to_tantivy_doc(result);
                    self.tantivy_writer.add_document(doc).await;
                }
            }
        }
        self.tantivy_writer.commit().await;

        // 3. Populate moka — instantly available for next autocomplete invocation
        self.moka_cache.insert(query, results).await;
    }
}
```

### `YtDlpPort` extension

Add to the existing port in `crates/application/src/ports/ytdlp.rs`:

```rust
/// Search YouTube and return top N results as metadata.
/// Internally uses "ytsearch{n}:{query}".
async fn search_top_n(
    &self,
    query: &str,
    n:     usize,
) -> Result<Vec<VideoMetadata>, AppError>;
```

Implementation uses `ytsearch5:{query}` with `--dump-json --no-playlist`.
Output is ndjson — one JSON object per result line (same parse path as
single video, not flat-playlist). Parse up to `n` lines.

---

## Moka Cache Configuration

In `apps/bot/src/main.rs` or a shared cache module:

```rust
use moka::future::Cache;

let youtube_search_cache: Arc<Cache<String, Vec<YoutubeSearchResult>>> =
    Arc::new(
        Cache::builder()
            .max_capacity(500)
            .time_to_live(Duration::from_secs(900))   // 15 minutes
            .time_to_idle(Duration::from_secs(300))   // 5 min idle eviction
            .build()
    );
```

Cache key: lowercased, trimmed query string (same normalisation applied
everywhere the key is constructed).

---

## `YoutubeSearchRepository` Port

New port methods in `crates/application/src/ports/repository.rs`:

```rust
/// Upsert a search result into youtube_search_cache.
/// ON CONFLICT (video_id) DO UPDATE SET last_seen_at = now().
async fn upsert_search_result(
    &self,
    query:  &str,
    result: &VideoMetadata,
) -> Result<(), AppError>;

/// Look up a single search cache entry by video_id.
async fn find_search_cache_by_video_id(
    &self,
    video_id: &str,
) -> Result<Option<YoutubeSearchCacheRow>, AppError>;

/// Given a list of video_ids, return those that already exist in tracks.
/// Used to avoid indexing duplicates in Tantivy.
async fn find_existing_video_ids(
    &self,
    video_ids: &[&str],
) -> Result<HashSet<String>, AppError>;

/// Update youtube_search_cache.track_id when a search stub is downloaded.
async fn link_search_cache_to_track(
    &self,
    video_id: &str,
    track_id: Uuid,
) -> Result<(), AppError>;
```

---

## Tantivy Startup Indexing

The Tantivy index is rebuilt on startup from two sources. In
`crates/adapters-search/src/tantivy_indexer.rs` (or equivalent):

```rust
pub async fn rebuild_index(
    pool:         &PgPool,
    index_writer: &mut IndexWriter,
) -> Result<(), AppError> {
    // 1. Index all tracks (existing behaviour, now with source field)
    let tracks = sqlx::query_as!(TrackRow,
        "SELECT id, title, artist_display, source, youtube_video_id,
                youtube_uploader, duration_ms
         FROM tracks
         WHERE title IS NOT NULL"
    ).fetch_all(pool).await?;

    for track in tracks {
        index_writer.add_document(track_to_doc(&track))?;
    }

    // 2. Index youtube_search_cache — skip video_ids already in tracks
    let search_stubs = sqlx::query_as!(SearchCacheRow,
        "SELECT sc.*
         FROM youtube_search_cache sc
         WHERE NOT EXISTS (
             SELECT 1 FROM tracks t WHERE t.youtube_video_id = sc.video_id
         )"
    ).fetch_all(pool).await?;

    for stub in search_stubs {
        index_writer.add_document(search_stub_to_doc(&stub))?;
    }

    index_writer.commit()?;
    Ok(())
}
```

---

## Submission Handler Changes

The `/play` command receives the autocomplete `value` field on submission,
which is now one of three things:

```rust
fn classify_submission_value(value: &str) -> SubmissionValue {
    // UUID → local track or YouTube-downloaded stub (fast path)
    if let Ok(uuid) = Uuid::parse_str(value) {
        return SubmissionValue::TrackId(uuid);
    }
    // YouTube URL → full Pass 1 flow
    if let Some(video_id) = extract_youtube_video_id(value) {
        return SubmissionValue::YoutubeUrl(value.to_string());
    }
    // yt: prefix wasn't resolved to a UUID — treat as YouTube search
    if value.starts_with("yt:") {
        return SubmissionValue::YoutubeSearch(
            value.trim_start_matches("yt:").trim().to_string()
        );
    }
    // Raw text not resolved via autocomplete (user typed & submitted)
    SubmissionValue::RawQuery(value.to_string())
}
```

When `SubmissionValue::YoutubeUrl` is received and the video_id maps to
an existing `youtube_search_cache` entry (but not yet in `tracks`), call
`link_search_cache_to_track` after the download stub is created, to keep
the search cache row associated with the eventual track.

---

## New Crate Structure

No new crate is required. Changes are distributed as:

| Crate | Change |
|-------|--------|
| `crates/domain` | New types: `YoutubeSearchResult`, `YoutubeSearchCacheRow`, `AutocompleteMode`, `SubmissionValue` |
| `crates/application` | New worker: `YoutubeSearchWorker`; new port methods on `YoutubeSearchRepository`; extend `YtDlpPort` with `search_top_n` |
| `crates/adapters-ytdlp` | Implement `search_top_n` (ndjson parse path) |
| `crates/adapters-persistence` | New `YoutubeSearchRepository` impl; updated Tantivy indexer (two sources) |
| `crates/adapters-search` | Tantivy schema extended; startup rebuild reads two tables; incremental add for search results; dedup on video_id |
| `crates/adapters-discord` | Autocomplete handler rewritten; `classify_autocomplete_input`; `format_choice`; URL resolved preview; `UserAutocompleteState` DashMap; `classify_submission_value` in play handler |
| `apps/bot` | Moka cache instantiation; `YoutubeSearchWorker` wiring |
| `migrations/` | New `0008_youtube_search_cache.sql` |

---

## Verification Plan

```bash
# Schema:
cargo sqlx migrate run
psql -c "\d youtube_search_cache"
cargo sqlx prepare --workspace
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# Display format — verify 100-char truncation:
# Track with very long title + artist should show "…" at end
# Track with NULL artist_display but uploader → shows uploader
# Track with NULL artist_display AND NULL uploader → title only, no separator

# Autocomplete stability trigger:
# /play neve (first invocation) → local results only, no background fetch yet
# /play neve (same query again) → triggers background fetch
# /play neve (third invocation, ~3s later) → YouTube results appear in list
# SELECT * FROM youtube_search_cache WHERE query LIKE '%neve%';
# → rows should exist

# yt: prefix:
# /play yt:rick astley → shows only YouTube results (no local tracks)
# Background fetch triggers unconditionally on first invocation

# URL resolved preview:
# Paste https://www.youtube.com/watch?v=dQw4w9WgXcQ (already downloaded)
# → shows "▶ Never Gonna Give You Up — Rick Astley · 3:33"
# Paste https://www.youtube.com/watch?v=UNKNOWN_ID (not in DB)
# → shows "▶ YouTube: UNKNOWN_ID"

# Heuristic merge:
# Library with 25+ tracks: search term matching all → no YouTube results shown
# Library with 3 matching tracks: YouTube fills remaining (up to 20 more)
# Library with 0 matches: all 25 slots are YouTube results

# Deduplication:
# Video already in tracks table → appears once with source="youtube" (not duplicated from search cache)

# Tantivy dedup on download complete:
# 1. Search for a video → appears as "youtube_search" in autocomplete
# 2. /play {url} → download completes
# 3. Search same query → same video now appears as "youtube" (downloaded), not "youtube_search"
# No duplicate entries in autocomplete for the same video

# Submission from search stub:
# /play (select a youtube_search result from autocomplete)
# → plays correctly, link_search_cache_to_track called
# SELECT track_id FROM youtube_search_cache WHERE video_id = '...';
# → should be non-null after play
```
