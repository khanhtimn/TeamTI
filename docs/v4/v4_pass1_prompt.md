# TeamTI v4 — Pass 1 Implementation Prompt
## YouTube Playback: Hybrid Stream-While-Cache

> Attach alongside: `teamti_v4_pass1_design.md`
> Also attach: all v3 output files, full migrations directory,
> `state.rs`, `lifecycle_worker.rs`, enrichment worker, analysis
> worker, and the existing `/play` command file.
>
> The design spec is authoritative for all types, SQL, flow logic,
> and config names. This prompt provides goals and acceptance criteria.

---

## What This Pass Builds

When this pass is done, typing `/play https://youtube.com/watch?v=...`
works exactly like `/play <local track>` — from the user's perspective,
there is no difference. The bot responds with "Added to queue: {title}",
the track plays, appears in listen history, can be favourited, and
eventually surfaces in recommendations. The mechanics of streaming vs
local file are entirely invisible.

The two-phase lifecycle is:

**Phase 1 — Cold play (first time a URL is seen):**
The bot fetches metadata (1–3s), queues the track for immediate
streaming via Songbird's `YoutubeDl` input, and simultaneously starts
a background download. The user hears audio within ~2–4 seconds.

**Phase 2 — Warm play (subsequent plays, after download):**
The cache lookup hits immediately. The track plays from the local opus
file, exactly like a NAS track. Response time is under 500ms.

The only operational difference users ever see is that first-play
response time is slightly longer for YouTube tracks than local tracks.

---

## Critical Implementation Constraints

### yt-dlp is a subprocess — `tokio::process::Command` only

Do not use any Rust yt-dlp crate. Invoke the binary directly via
`tokio::process::Command`. This gives full control over arguments,
avoids crate version drift behind yt-dlp releases, and lets you handle
stderr output explicitly for error detection.

```rust
// Correct pattern:
let output = tokio::process::Command::new(&config.ytdlp_binary)
    .args(&["--dump-json", "--no-playlist", "--no-warnings", url])
    .output()
    .await?;

if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(AppError::YouTube { ... });
}

let metadata: YtDlpJson = serde_json::from_slice(&output.stdout)?;
```

Always capture stderr and include it in error context. yt-dlp's error
messages are the primary debugging tool for bot detection issues.

### Enrichment and analysis workers MUST be gated on `blob_location IS NOT NULL`

This is the most critical correctness fix in the pass. Both the
enrichment worker poll query and the analysis worker poll query must
add `AND blob_location IS NOT NULL` before they touch YouTube stubs.
See the design spec for the exact SQL. Without this fix, the workers
will hammer YouTube stubs on every poll cycle, exhaust their attempt
counters, and mark valid tracks as permanently failed before their
files even finish downloading.

### The Semaphore is the download concurrency mechanism — no separate queue

`YoutubeDownloadWorker` uses a `tokio::sync::Semaphore` with
`YTDLP_DOWNLOAD_CONCURRENCY` permits. `schedule()` spawns a tokio
task immediately for every requested download; the task blocks on
`semaphore.acquire()` until a slot is free. This means:

- No download queue to manage
- No polling loop
- Natural backpressure: if `LOOKAHEAD_DEPTH = 3` and `CONCURRENCY = 2`,
  at most 2 downloads run concurrently; the 3rd waits for a permit

### `create_youtube_stub` must be idempotent

Two users can request the same YouTube URL simultaneously. Both see
`youtube_download_jobs` cache miss, both call `create_youtube_stub`.
Use `INSERT ... ON CONFLICT (youtube_video_id) DO NOTHING RETURNING id`.
If the INSERT returns no row (conflict), query for the existing track_id.
The subsequent `upsert_download_job` uses `ON CONFLICT (video_id) DO NOTHING`.

### stream_url expiry check

Before queueing a streaming input for a cached-but-not-downloaded track
(status = 'pending' or 'downloading'), check `stream_url_expires_at`.
If within 30 minutes of expiry (or already expired), re-fetch metadata
via `fetch_video_metadata`. This adds 1-3s but prevents a Songbird
input that immediately fails with an HTTP 403.

### `/play` must defer immediately

Discord slash command interactions expire after 3 seconds without a
response. The metadata fetch takes 1-3 seconds. Call `ctx.defer().await`
as the very first line of the play handler, before any async work.

### Blob path directory creation

`$MEDIA_ROOT/youtube/{uploader}/{album}/` directories may not exist.
Call `tokio::fs::create_dir_all(output_path.parent().unwrap())` before
spawning the yt-dlp download process.

---

## What This Pass Does NOT Do

- No progress bar or download status shown to users (fully transparent)
- No NP message changes — YouTube tracks post the same NP embed as local tracks
- No cache eviction (manual only — no TTL, no LRU)
- No `/youtube` subcommand group — everything goes through `/play`
- No per-guild download limits (concurrency is global)
- No video/livestream support — audio only
- No age-restricted content handling beyond cookies

---

## Definition of Done

1. `cargo sqlx migrate run` applies cleanly. New columns and table visible.
2. `cargo sqlx prepare --workspace` passes. `cargo build --workspace`
   produces zero errors and zero warnings.
3. `/play https://www.youtube.com/watch?v=dQw4w9WgXcQ` responds with
   "Added to queue: Never Gonna Give You Up" within 4 seconds.
   Audio begins streaming immediately after.
4. Thirty seconds after step 3, `SELECT blob_location FROM tracks
   WHERE youtube_video_id = 'dQw4w9WgXcQ'` returns a non-null path.
   The file exists on disk at `$MEDIA_ROOT/{path}`.
5. Running `/play` with the same URL a second time responds in under
   500ms and plays from the local file (confirmed by absence of any
   Songbird `YoutubeDl` input in logs).
6. `/play https://www.youtube.com/playlist?list=...` queues all tracks.
   Only the first `YTDLP_LOOKAHEAD_DEPTH` undownloaded tracks have
   `youtube_download_jobs.status IN ('downloading', 'done')`.
   Remaining entries have `status = 'pending'`.
7. `/play some obscure query` with no local matches queues the YouTube
   top result automatically.
8. A YouTube track appears in `/queue`, shows listen events after
   playback, and can be added to favourites.
9. Tracks with `blob_location IS NULL` are never picked up by the
   enrichment or analysis workers (verify `enrichment_attempts = 0`
   for any stub with `blob_location IS NULL` after 60 seconds).
10. After setting a download job to `attempts = YTDLP_MAX_DOWNLOAD_ATTEMPTS`
    and triggering a failure: a stub with no listen_events is deleted.
    A stub with listen_events is kept with `blob_location = NULL`.
