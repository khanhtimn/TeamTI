# TeamTI v4 — Pass 2 Implementation Prompt
## Unified Search: Tantivy Autocomplete with YouTube Integration

> Attach alongside: `teamti_v4_pass2_design.md`
> Also attach: Pass 1 output, all migrations, the existing Tantivy
> integration code, `/play` command, and autocomplete handler.
>
> The design spec is authoritative for all types, SQL, and flow logic.

---

## What This Pass Builds

When this pass is done, the `/play` autocomplete is unified across all
content sources. A user searching "never gonna" sees local tracks AND
previously-seen YouTube results in a single ranked list. On the second
invocation of the same query, fresh YouTube search results appear
automatically — with no user action beyond re-triggering autocomplete.

The user never thinks about "local vs YouTube." They just see the best
matching results, formatted consistently, ranked by relevance. The
`yt:` prefix provides a deliberate escape hatch for YouTube-only search.

---

## Critical Implementation Constraints

### Never call yt-dlp from the autocomplete hot path

The autocomplete handler must respond in under 3 seconds or Discord
silently drops the response. yt-dlp metadata or search calls take 2–4s.
The rule is absolute: **no yt-dlp subprocess is ever spawned inside
`handle_autocomplete`**. All yt-dlp work happens in `tokio::spawn`
fire-and-forget tasks, always after the autocomplete response is sent.

### Query stability — not a timer

The background fetch trigger is NOT a `tokio::sleep` debounce. It fires
when the same query string appears twice consecutively from the same user.
Discord's autocomplete invokes the handler on every keystroke at its own
rate. Sending the same string twice means the user paused naturally.
No timers, no `tokio::sleep`.

### Tantivy write path must not block autocomplete reads

Tantivy uses a single `IndexWriter` for all writes. If `commit()` is
called while an autocomplete read is in progress, Tantivy handles this
correctly (reads use a snapshot). However, the writer must be shared
carefully — use an `Arc<Mutex<IndexWriter>>` and only hold the lock for
the duration of `add_document` + `commit`. Never hold it across an
`await` point.

### Moka cache key normalisation is the deduplication contract

The moka cache key, the `youtube_search_cache.query` column, and the
query stability comparison must all use the same normalisation:
`query.trim().to_lowercase()`. If any of the three use a different
form, the stability check will fail or the cache will miss on repeated
queries.

### Submission value routing must handle all three forms

The `/play` handler receives one of: a UUID (local/downloaded track),
a YouTube URL, or a raw query string. The autocomplete value field
encodes which path to take. The `classify_submission_value` function
is the single entry point — no other code should inspect the raw value.
See design spec for the full routing table.

---

## What This Pass Does NOT Do

- No changes to existing playback, queue management, or NP display
- No YouTube search cache cleanup/eviction (deferred to a later pass)
- No per-guild search result filtering
- No search result ranking by play count (verify it's already in Tantivy
  from v3; if missing, add `play_count` and `last_played_at` to the
  Tantivy schema and scoring formula — see Q7 from design discussions)
- No search history or "recent plays" surface in autocomplete
- No changes to the `tracks` table beyond what Pass 1 already added

---

## Definition of Done

1. `cargo sqlx migrate run` applies cleanly. `youtube_search_cache`
   table visible with correct columns and indexes.
2. `cargo build --workspace` — zero errors, zero warnings.
3. Typing a query that matches local tracks: local results appear
   immediately, formatted as `🎵 Title — Artist · M:SS`.
4. Typing a query with no local matches: first invocation shows empty
   or hint only. Second invocation of same query triggers background
   fetch. Third invocation (after ~3-4s) shows `📺` YouTube results.
5. `yt:rick astley` → only YouTube results shown, background fetch
   triggered on first invocation (not second).
6. Pasting `https://www.youtube.com/watch?v=dQw4w9WgXcQ` for a
   downloaded track → shows resolved `▶ Never Gonna Give You Up —
   Rick Astley · 3:33` (not the raw URL).
7. Pasting a valid YouTube URL not yet in the DB → shows
   `▶ YouTube: {video_id}`.
8. `SELECT count(*) FROM youtube_search_cache` increases after
   performing searches.
9. Tantivy dedup: a video that exists in both `tracks` and
   `youtube_search_cache` appears exactly once in autocomplete
   results, with source indicator matching `tracks.source`.
10. Selecting a `youtube_search` result from autocomplete and
    submitting → plays correctly via Pass 1 flow, and
    `youtube_search_cache.track_id` is populated after download.
