# TeamTI v4 — Audit Pass 1.1
## YouTube Hybrid Architecture: Correctness, Bug & Edge Case Review

> **Scope.** Full review of the Pass 1 output across all affected crates.
> Read every new file in `adapters-ytdlp`, `youtube_worker.rs`, the
> updated `/play` command, and the migration. Fix all Critical and Major
> findings. Apply all Optimizations. Self-Explore items require reading
> the implementation — fix if broken, document if accepted.
>
> **Run before starting:**
> ```bash
> cargo build --workspace 2>&1 | grep -E "^error|^warning"
> yt-dlp --version   # verify binary is present
> cargo sqlx prepare --workspace
> ```

---

## Findings Index

| ID | Severity | Area | Title |
|----|----------|------|-------|
| C1 | Critical | `adapters-ytdlp` | `YoutubeDl::new` receives signed stream URL — must be video page URL |
| C2 | Critical | `adapters-ytdlp` | Playlist `--dump-json` output is ndjson — `serde_json::from_slice` fails |
| C3 | Critical | `adapters-ytdlp` | `--flat-playlist` returns `uploader: null` — stubs have incomplete metadata; download worker must repair before constructing blob path |
| C4 | Critical | `youtube_worker.rs` | yt-dlp subprocess not killed on task cancellation — orphaned processes on shutdown |
| C5 | Critical | Enrichment/Analysis | `blob_location IS NOT NULL` guard — verify both worker queries were actually updated |
| M1 | Major | `/play` | Playlist stub creation is sequential — 200 single INSERTs for large playlists |
| M2 | Major | `adapters-ytdlp` | Shorts URL not normalised to `watch?v=` — same video gets two `youtube_video_id` entries |
| M3 | Major | `youtube_worker.rs` | Download worker doesn't classify yt-dlp errors — transient vs permanent failures treated identically |
| M4 | Major | `adapters-ytdlp` | No timeout on yt-dlp subprocess — hangs forever on network stall |
| M5 | Major | `lifecycle_worker.rs` | `trigger_lookahead` panics if `YTDLP_LOOKAHEAD_DEPTH` > `queue_meta.len()` |
| M6 | Major | `/play` | `/play` interaction not deferred before metadata fetch — Discord 3s timeout breach |
| O1 | Optim. | `adapters-ytdlp` | `YoutubeDl::new_ytdl_like` exists — use custom binary path instead of relying on `$PATH` |
| O2 | Optim. | `adapters-ytdlp` | `YoutubeDl::new_search` exists in Songbird — use it for search fallback streaming instead of sentinel hack |
| O3 | Optim. | `apps/bot` | yt-dlp binary version check on startup — emit clear error before any YouTube command fails |
| O4 | Optim. | `youtube_worker.rs` | Input upgrade: when download completes, reroute mid-queue YoutubeDl entries to local File |
| S1 | Explore | `adapters-ytdlp` | yt-dlp error string classification — map stderr patterns to user-friendly messages |
| S2 | Explore | `/play` | Large playlist response time — `--flat-playlist` for 500+ tracks may exceed Discord interaction deadline |
| S3 | Explore | Queue embed | Incomplete stub metadata in queue display — artist = NULL looks bad |
| S4 | Explore | `youtube_worker.rs` | Duplicate video_id within same playlist — verify idempotency end-to-end |
| S5 | Explore | Shutdown | Active download tasks not awaited on graceful shutdown |

---

## Critical Fixes

### C1 — `YoutubeDl::new` must receive the video page URL, not the signed stream URL

**File:** `crates/adapters-discord/src/commands/play.rs` (or wherever Songbird input is constructed)

**Problem.** The design spec and likely the implementation construct the Songbird input as:

```rust
// WRONG:
songbird::input::YoutubeDl::new(client, stream_url)
// where stream_url = "https://rr4---sn-xxx.googlevideo.com/videoplayback?..."
```

Songbird's `YoutubeDl` documentation states it is "a lazily instantiated call
to download a file, **finding its URL via youtube-dl**." It calls yt-dlp
internally on whatever URL you pass. Passing a signed CDN audio URL causes
yt-dlp to receive a `googlevideo.com` URL — not a recognised YouTube page.
yt-dlp has no extractor for that domain and returns an error, causing playback
to fail immediately.

The `stream_url` from `--dump-json` is useful for one thing only: the download
worker uses it to specify an exact format ID when invoking yt-dlp for the
audio download. It is **not** a Songbird input.

**Fix.** Always pass the canonical video page URL to `YoutubeDl::new`:

```rust
// CORRECT — for any YouTube video:
let page_url = format!("https://www.youtube.com/watch?v={}", video_id);
let input = songbird::input::YoutubeDl::new(client.clone(), page_url);
```

For playlists: each entry's `url` field from `--flat-playlist` is already
the canonical video page URL (`https://www.youtube.com/watch?v=VIDEO_ID`).
Use it directly.

The `stream_url` column on `youtube_download_jobs` is retained for the
download worker's `yt-dlp -f FORMAT_ID` invocation only. Update any
comments that describe it as the Songbird input source.

---

### C2 — Playlist `--dump-json` output is ndjson; `serde_json::from_slice` fails

**File:** `crates/adapters-ytdlp/src/lib.rs` — `fetch_playlist_metadata`

**Problem.** When `--flat-playlist --dump-json` is used, yt-dlp writes one
JSON object per line to stdout (newline-delimited JSON / ndjson). When
`--dump-json` is used for a single video, it writes a single JSON object.
`serde_json::from_slice(&output.stdout)` on ndjson output returns
`Error("trailing characters", ...)` after parsing the first object.

**Fix.** Parse each output format correctly:

```rust
// Single video — one JSON object:
pub async fn fetch_video_metadata(&self, url: &str) -> Result<VideoMetadata, AppError> {
    let output = self.run_ytdlp(&["--dump-json", "--no-playlist", "--no-warnings", url]).await?;
    let raw: YtDlpRawEntry = serde_json::from_slice(&output.stdout)
        .map_err(|e| AppError::YouTube {
            kind: YoutubeErrorKind::InvalidMetadata,
            detail: e.to_string(),
        })?;
    Ok(raw.into())
}

// Playlist — ndjson: one JSON object per line:
pub async fn fetch_playlist_metadata(&self, url: &str) -> Result<Vec<VideoMetadata>, AppError> {
    let output = self.run_ytdlp(&[
        "--dump-json", "--flat-playlist", "--no-warnings", url,
    ]).await?;

    let mut entries = Vec::new();
    for line in output.stdout.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() { continue; }
        match serde_json::from_slice::<YtDlpFlatEntry>(line) {
            Ok(entry) => entries.push(entry.into()),
            Err(e) => tracing::warn!(error = %e, "skipping malformed playlist entry"),
        }
    }
    Ok(entries)
}
```

Two separate structs for deserialization:

```rust
/// Full metadata from --dump-json (single video): has formats, uploader, etc.
#[derive(serde::Deserialize)]
struct YtDlpRawEntry {
    id:            String,
    webpage_url:   String,
    title:         String,
    uploader:      Option<String>,
    channel_id:    Option<String>,
    duration:      Option<f64>,    // seconds, may be float
    thumbnail:     Option<String>,
    album:         Option<String>,
    track:         Option<String>,
    artist:        Option<String>,
    // Best audio format URL — used only by download worker
    // Extract from formats[] array: format with audio_ext = "opus"
    // and abr highest, or use url field if format is audio-only
    #[serde(skip)]
    _formats:      (),             // handled separately if stream_url needed
}

/// Minimal metadata from --flat-playlist: uploader is always null
#[derive(serde::Deserialize)]
struct YtDlpFlatEntry {
    id:       String,
    url:      String,            // the canonical watch?v= URL
    title:    Option<String>,
    duration: Option<f64>,
    // uploader: always null in flat entries — intentionally omitted
}
```

---

### C3 — `--flat-playlist` returns `uploader: null` — download worker must repair stub before constructing blob path

**Files:** `crates/adapters-ytdlp/src/lib.rs`, `crates/application/src/youtube_worker.rs`

**Problem.** Confirmed from yt-dlp source and issues: `--flat-playlist`
intentionally omits `uploader`, `channel_id`, `thumbnail`, `album`, and
`artist` fields (all are `null`). Flat entries only reliably have `id`,
`url`, `title`, and `duration`.

This creates two downstream bugs:

**Bug A: Wrong blob path.** The blob path constructor uses `uploader`
as the directory name. With `uploader = null`, the sanitize function
falls back to `"Unknown"`, producing:
```
youtube/Unknown/Never Gonna Give You Up_dQw4w9WgXcQ.opus
```
...even for well-known artists. If two uploads from different artists
both have `uploader = null` from a flat playlist, they collide into
the same `Unknown/` directory.

**Bug B: Missing thumbnail in NP embed.** The NP embed uses
`youtube_thumbnail_url`. For playlist stubs, this is NULL until the
download worker repairs it.

**Fix.** The download worker has a mandatory pre-download metadata
repair step for flat-playlist stubs. Add a field
`metadata_complete: bool` (or check `youtube_uploader IS NULL`) to
detect incomplete stubs:

```rust
// In youtube_worker.rs, run_download():
// After acquiring semaphore, before downloading:

// Check if stub has complete metadata (non-null uploader)
let track = repo.get_track_by_youtube_video_id(&video_id).await?;
let (full_meta, final_blob_path) = if track.youtube_uploader.is_none() {
    // Flat playlist stub — fetch individual metadata to repair
    let meta = ytdlp.fetch_video_metadata(&url).await?;

    // Update the stub with complete metadata
    repo.update_youtube_stub_metadata(&video_id, &meta).await?;

    let blob_path = youtube_blob_path(&meta);
    (meta, blob_path)
} else {
    // Single video stub — blob path already correct from /play handler
    let blob_path = track.blob_location.clone()
        .unwrap_or_else(|| youtube_blob_path_from_track(&track));
    (VideoMetadata::from_track(&track), blob_path)
};
```

The `update_youtube_stub_metadata` method updates `youtube_uploader`,
`youtube_channel_id`, `youtube_thumbnail_url`, and `artist_display`
on the `tracks` row.

Additionally, add `update_youtube_stub_metadata` to `YoutubeRepository`:

```rust
async fn update_youtube_stub_metadata(
    &self,
    video_id: &str,
    meta:     &VideoMetadata,
) -> Result<(), AppError>;
```

---

### C4 — yt-dlp subprocess not killed on task cancellation

**File:** `crates/adapters-ytdlp/src/lib.rs`

**Problem.** yt-dlp is a CPU+network intensive subprocess. If the tokio
runtime begins shutdown while a download is active (Ctrl+C, panic, etc.),
`tokio::process::Command::output().await` continues blocking the runtime
shutdown until the yt-dlp process finishes — potentially minutes for a
large file. Tokio will eventually force-kill the task, but the yt-dlp
process itself becomes orphaned (no longer supervised).

**Fix.** Use `spawn()` + `wait()` with `.kill_on_drop(true)` instead
of `output()` for the download invocation:

```rust
// In download_audio():
let mut child = tokio::process::Command::new(&self.binary_path)
    .args(&["-f", &format_selector, "-o", &output_path_str, url])
    .kill_on_drop(true)   // ← ensures child is killed when Child is dropped
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .map_err(|e| AppError::YouTube {
        kind:   YoutubeErrorKind::SpawnFailed,
        detail: e.to_string(),
    })?;

let status = child.wait().await
    .map_err(|e| AppError::YouTube {
        kind:   YoutubeErrorKind::ProcessFailed,
        detail: e.to_string(),
    })?;

if !status.success() {
    let stderr = if let Some(mut s) = child.stderr.take() {
        let mut buf = String::new();
        let _ = tokio::io::AsyncReadExt::read_to_string(&mut s, &mut buf).await;
        buf
    } else { String::new() };

    return Err(classify_ytdlp_error(&stderr));
}
```

For the metadata fetch (`--dump-json`, short-lived), `output()` is
acceptable — it's quick enough that kill_on_drop isn't critical.

---

### C5 — `blob_location IS NOT NULL` guard — verify both worker queries were updated

**Files:** `crates/adapters-persistence/src/repositories/track_repository.rs`

**Action.** Search for both worker poll queries and confirm the guard
is present:

```bash
# Must return two results, one for each worker:
grep -n "blob_location IS NOT NULL" \
    crates/adapters-persistence/src/repositories/track_repository.rs
```

If either query is missing the guard, add it exactly as specified in
the design spec. This is non-negotiable — without it, the enrichment
worker hammers YouTube stubs every poll cycle and exhausts their retry
counters before the download finishes.

Also verify the existing index supports filtered queries efficiently:

```sql
-- The analysis and enrichment queue indexes should include the guard:
-- (already specified in design spec — verify they were applied)
CREATE INDEX IF NOT EXISTS idx_tracks_enrichment_queue
ON tracks(enrichment_status, enrichment_attempts, enriched_at)
WHERE enrichment_locked = false
  AND enrichment_status IN ('pending', 'failed', 'low_confidence', 'unmatched')
  AND blob_location IS NOT NULL;   -- ← verify this was added
```

---

## Major Fixes

### M1 — Playlist stub creation is sequential; batch it

**File:** `crates/adapters-persistence/src/repositories/youtube_repository.rs`

**Problem.** For a 50-track playlist with all cache misses, the current
implementation likely creates stubs one by one:

```rust
for entry in &entries {
    repo.create_youtube_stub(entry).await?;  // 50 round-trips
}
```

50 sequential DB round-trips inside a Discord interaction handler is slow
and holds the interaction open longer than needed.

**Fix.** Batch insert all stubs in a single query using `UNNEST`:

```sql
INSERT INTO tracks (
    id, title, artist_display, duration_ms, blob_location, source,
    youtube_video_id, youtube_uploader, youtube_thumbnail_url,
    enrichment_status, analysis_status, created_at, updated_at
)
SELECT
    gen_random_uuid(),
    UNNEST($1::text[]),   -- titles
    UNNEST($2::text[]),   -- artist_display (NULL for flat entries)
    UNNEST($3::int[]),    -- duration_ms
    NULL,                 -- blob_location: always NULL for new stubs
    'youtube',
    UNNEST($4::text[]),   -- youtube_video_ids
    NULL,                 -- youtube_uploader: NULL for flat entries, repaired later
    NULL,                 -- youtube_thumbnail_url: NULL for flat entries
    'pending',            -- enrichment_status: gated by blob_location IS NOT NULL
    'pending',            -- analysis_status: same
    now(), now()
ON CONFLICT (youtube_video_id) DO NOTHING
RETURNING id, youtube_video_id
```

The RETURNING clause gives back the mapping from `youtube_video_id` to
the assigned `track_id` (both new and existing rows from prior caching).
For the existing rows that weren't inserted (ON CONFLICT DO NOTHING),
do a follow-up `SELECT id, youtube_video_id FROM tracks WHERE youtube_video_id = ANY($1)`.

Also batch-insert the download jobs:

```sql
INSERT INTO youtube_download_jobs (video_id, track_id, url, status, created_at)
SELECT
    UNNEST($1::text[]),   -- video_ids
    UNNEST($2::uuid[]),   -- track_ids (from the stub creation step)
    UNNEST($3::text[]),   -- urls
    'pending',
    now()
ON CONFLICT (video_id) DO NOTHING
```

---

### M2 — YouTube Shorts URL not normalised — same video gets two stubs

**File:** `crates/adapters-ytdlp/src/lib.rs` or the URL classification logic

**Problem.** These four URLs all refer to the same video:
```
https://www.youtube.com/watch?v=dQw4w9WgXcQ
https://youtu.be/dQw4w9WgXcQ
https://www.youtube.com/shorts/dQw4w9WgXcQ
https://music.youtube.com/watch?v=dQw4w9WgXcQ
```

If the deduplication key is `youtube_video_id` and we extract it correctly
from all four URL forms, the cache lookup works. But if any code path uses
the raw URL (e.g., the `url` column in `youtube_download_jobs`) for
subsequent yt-dlp invocations, inconsistency creeps in.

**Fix.** Normalise all YouTube URLs to canonical form immediately after
URL classification, before any DB operations or yt-dlp invocations:

```rust
/// Extract the video_id from any supported YouTube URL form.
/// Returns None if the URL is not a recognised YouTube video URL.
pub fn extract_youtube_video_id(url: &str) -> Option<String> {
    // youtu.be/VIDEO_ID
    if let Some(path) = url.strip_prefix("https://youtu.be/") {
        return Some(path.split('?').next()?.to_string());
    }

    // Parse query string for ?v=VIDEO_ID
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;

    if matches!(host, "www.youtube.com" | "youtube.com" | "music.youtube.com") {
        let path = parsed.path();
        if path.starts_with("/shorts/") {
            return Some(path.trim_start_matches("/shorts/").to_string());
        }
        return parsed.query_pairs()
            .find(|(k, _)| k == "v")
            .map(|(_, v)| v.into_owned());
    }
    None
}

/// Canonical video URL — always use this for yt-dlp and DB storage.
pub fn canonical_youtube_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={}", video_id)
}
```

Use `canonical_youtube_url(video_id)` everywhere a URL is stored or
passed to yt-dlp. The raw input URL is discarded after `video_id`
extraction.

---

### M3 — yt-dlp errors not classified — transient vs permanent failures treated identically

**File:** `crates/adapters-ytdlp/src/lib.rs`

**Problem.** Currently all yt-dlp non-zero exit codes likely return
`AppError::YouTube { kind: ProcessFailed, detail: stderr }`. This treats
a network timeout (retry in 30s) the same as "video not available" (never
retry — mark permanently failed immediately).

**Fix.** Classify stderr patterns into error kinds:

```rust
pub fn classify_ytdlp_error(stderr: &str) -> AppError {
    let s = stderr.to_lowercase();

    let kind = if s.contains("sign in") || s.contains("confirm you're not a bot")
                   || s.contains("login required") {
        YoutubeErrorKind::AuthRequired     // → suggest adding cookies file
    } else if s.contains("video unavailable") || s.contains("this video has been removed")
                   || s.contains("account has been terminated") {
        YoutubeErrorKind::VideoUnavailable // → permanent failure immediately
    } else if s.contains("private video") {
        YoutubeErrorKind::PrivateVideo     // → permanent failure
    } else if s.contains("not available in your country")
                   || s.contains("geo") {
        YoutubeErrorKind::GeoBlocked       // → permanent failure
    } else if s.contains("http error 429") || s.contains("too many requests") {
        YoutubeErrorKind::RateLimited      // → transient, back off
    } else if s.contains("urlopen error") || s.contains("connection") {
        YoutubeErrorKind::NetworkError     // → transient, retry
    } else {
        YoutubeErrorKind::Unknown          // → retry up to max_attempts
    };

    AppError::YouTube { kind, detail: stderr.to_string() }
}
```

In `youtube_worker.rs`, use the kind to decide retry vs immediate permanent failure:

```rust
Err(AppError::YouTube { kind, .. }) => {
    use YoutubeErrorKind::*;
    let is_permanent = matches!(kind,
        VideoUnavailable | PrivateVideo | GeoBlocked
    );

    if is_permanent || attempts + 1 >= self.max_attempts {
        repo.permanently_fail_download_job(&video_id, &err.to_string()).await?;
        tracing::error!(video_id, %kind, "youtube download permanently failed");
    } else {
        repo.fail_download_job(&video_id, &err.to_string()).await?;
        tracing::warn!(video_id, %kind, attempts, "youtube download failed, will retry");
    }
}
```

Also emit a user-visible ephemeral message when the track is being played
and `permanently_failed`:

```rust
// When track_id's job has status = 'permanently_failed':
return Err(AppError::Command(match kind {
    YoutubeErrorKind::AuthRequired =>
        "This video requires sign-in. Set YTDLP_COOKIES_FILE to enable it.".into(),
    YoutubeErrorKind::VideoUnavailable | YoutubeErrorKind::PrivateVideo =>
        "This video is no longer available on YouTube.".into(),
    YoutubeErrorKind::GeoBlocked =>
        "This video is not available in your region.".into(),
    _ =>
        "This YouTube video could not be played.".into(),
}))
```

---

### M4 — No timeout on yt-dlp subprocess — hangs forever on network stall

**File:** `crates/adapters-ytdlp/src/lib.rs`

**Problem.** A stalled YouTube CDN connection or a hung yt-dlp process
will block the download task indefinitely. With `YTDLP_DOWNLOAD_CONCURRENCY
= 2`, two hung tasks starve all other downloads permanently.

**Fix.** Wrap the yt-dlp invocation in `tokio::time::timeout`:

```rust
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600); // 10 min max

// For metadata fetch:
let output = tokio::time::timeout(
    METADATA_TIMEOUT,
    child.wait_with_output(),
).await
.map_err(|_| AppError::YouTube {
    kind: YoutubeErrorKind::Timeout,
    detail: "metadata fetch timed out after 30s".into(),
})??;

// For download:
let status = tokio::time::timeout(
    DOWNLOAD_TIMEOUT,
    child.wait(),
).await
.map_err(|_| {
    // Kill the child on timeout
    let _ = child.kill();  // best-effort
    AppError::YouTube {
        kind: YoutubeErrorKind::Timeout,
        detail: "download timed out after 10 minutes".into(),
    }
})??;
```

`YoutubeErrorKind::Timeout` → treated as a transient error (retry).
`DOWNLOAD_TIMEOUT` is generous (10 minutes) to accommodate large files
on slow connections.

---

### M5 — `trigger_lookahead` panics if lookahead depth > queue length

**File:** `crates/application/src/youtube_worker.rs`

**Problem.**

```rust
// Likely implementation:
for entry in &queue_meta[1..=YTDLP_LOOKAHEAD_DEPTH] {
    // panics if queue_meta.len() <= YTDLP_LOOKAHEAD_DEPTH
}
```

**Fix.**

```rust
let end = (YTDLP_LOOKAHEAD_DEPTH + 1).min(queue_meta.len());
for entry in &queue_meta[1..end] {
    if entry.source == QueueSource::YouTube {
        if let Some(ref job) = get_download_job(&entry.track_id).await {
            if job.status != "done" {
                worker.schedule(
                    job.video_id.clone(),
                    canonical_youtube_url(&job.video_id),
                    expected_blob_path(&entry),
                );
            }
        }
    }
}
```

---

### M6 — `/play` interaction not deferred before metadata fetch

**File:** `crates/adapters-discord/src/commands/play.rs`

**Problem.** The design spec specifies `ctx.defer().await` as the first
call. If the implementation awaits the metadata fetch before deferring,
the interaction will timeout after 3 seconds and Discord shows
"The application did not respond" to the user — even though the track
eventually plays.

**Fix.** Verify the command handler structure is:

```rust
async fn handle_play(ctx: Context<'_>, input: String) -> Result<(), AppError> {
    ctx.defer().await?;                    // ← MUST be first, before any await
    match classify_input(&input) {
        PlayInput::YoutubeVideo(url) => {
            let video_id = extract_youtube_video_id(&url)
                .ok_or_else(|| AppError::Command("Invalid YouTube URL.".into()))?;
            handle_youtube_single(&ctx, &video_id).await?;
        }
        // ...
    }
    Ok(())
}
```

Any code path that calls yt-dlp must have `ctx.defer()` before the
yt-dlp invocation.

---

## Optimizations

### O1 — Use `YoutubeDl::new_ytdl_like` for custom binary path

**File:** Wherever `YoutubeDl::new` is called

Songbird provides `YoutubeDl::new_ytdl_like(program, client, url)` which
accepts a custom binary path. Use this with `config.ytdlp_binary` instead
of relying on the system `$PATH`:

```rust
let input = songbird::input::YoutubeDl::new_ytdl_like(
    &config.ytdlp_binary,      // e.g., "/usr/local/bin/yt-dlp"
    client.clone(),
    page_url,
);
```

This ensures the bot uses exactly the yt-dlp binary it was configured
with, even if a different version is on the system PATH.

---

### O2 — Use `YoutubeDl::new_search` for YouTube search fallback streaming

**File:** `crates/adapters-discord/src/commands/play.rs`

Songbird provides `YoutubeDl::new_search(client, query)` which internally
runs `ytsearch:{query}` via yt-dlp. For the search fallback streaming
input, this removes the need for the `__ytsearch__` sentinel:

```rust
// For the streaming input in search fallback:
let streaming_input = songbird::input::YoutubeDl::new_search(
    client.clone(),
    query.to_string(),
);

// Still call yt-dlp separately for stub creation:
let meta = ytdlp.search_top_result(query).await?;
// (creates stub, download job, etc.)
```

The `__ytsearch__` sentinel in autocomplete can remain as a signal to the
submission handler to take the YouTube search path. The sentinel is an
implementation detail, not a user-visible string.

---

### O3 — yt-dlp binary validation at startup

**File:** `apps/bot/src/main.rs`

Add a startup check before accepting any Discord commands:

```rust
// In main(), after config loading:
let ytdlp_version = tokio::process::Command::new(&config.ytdlp_binary)
    .arg("--version")
    .output()
    .await;

match ytdlp_version {
    Ok(output) if output.status.success() => {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        tracing::info!(version, binary = config.ytdlp_binary, "yt-dlp found");
    }
    Ok(_) | Err(_) => {
        tracing::warn!(
            binary = config.ytdlp_binary,
            "yt-dlp binary not found or not executable — YouTube commands will fail"
        );
        // Do not panic — bot should still serve local library
    }
}

if let Some(ref cookies) = config.ytdlp_cookies_file {
    if !std::path::Path::new(cookies).exists() {
        tracing::warn!(path = cookies, "YTDLP_COOKIES_FILE does not exist");
    }
}
```

---

### O4 — Input upgrade: reroute mid-queue YoutubeDl entries when download completes

**File:** `crates/application/src/youtube_worker.rs`, `crates/adapters-voice/src/state.rs`

**Context.** A track at position 3 in the queue was added as a `YoutubeDl`
streaming input (cache miss at queue time). The background download
completes before the track reaches position 0. The track will still stream
from YouTube rather than playing from the faster local file.

**Potential fix.** When `complete_download_job` fires, emit an event:

```rust
// In youtube_worker.rs after successful download:
if let Some(tx) = &self.lifecycle_tx {
    let _ = tx.send(TrackLifecycleEvent::YoutubeDownloadComplete {
        guild_id:      None,   // global — applies to all guilds
        video_id:      video_id.clone(),
        blob_location: blob_path.clone(),
    });
}
```

In `lifecycle_worker.rs`, handle `YoutubeDownloadComplete`:
1. For each active guild, scan `queue_meta` for entries matching `video_id`
   at positions > 0 (not currently playing)
2. For each match, call `TrackQueue::modify_queue` to swap the `YoutubeDl`
   input for a `File` input

**Note:** Swapping Songbird inputs mid-queue requires that the track hasn't
started yet (position > 0). Songbird's `modify_queue` provides direct
`Vec<Queued>` access, but swapping the underlying `Input` of a `Queued`
entry may not be directly supported. **Investigate whether this is feasible
in the current Songbird version.** If not, document it as a known limitation
and defer to v4 Pass 2.

---

## Self-Explore Items

### S1 — yt-dlp error string classification completeness

**Action.** Run the bot against known-bad videos and verify each error
type is correctly classified:

```bash
# Test each error class:
yt-dlp --dump-json "https://www.youtube.com/watch?v=DELETED_VIDEO_ID"
# → should produce VideoUnavailable pattern in stderr

yt-dlp --dump-json "https://www.youtube.com/watch?v=PRIVATE_VIDEO_ID"
# → should produce PrivateVideo pattern

yt-dlp --dump-json "https://www.youtube.com/watch?v=AGE_RESTRICTED_ID"
# → with no cookies: should produce AuthRequired pattern
```

Capture the actual stderr for each case and verify that `classify_ytdlp_error`
maps them to the correct `YoutubeErrorKind`. Update the pattern matching
if the actual stderr strings differ from the patterns in M3.

---

### S2 — Large playlist response time may exceed Discord interaction deadline

**Context.** `ctx.defer()` extends the response window to 15 minutes.
So the 3-second timeout is not a concern once deferred. However:

- `--flat-playlist` for a 1000-track playlist makes many YouTube API
  calls and can take 10-30 seconds
- The "Added N tracks to queue" followup message should be sent as soon
  as the metadata fetch completes, not after all stubs are created

**Verify:** Does the current implementation send the followup message
before or after the batch stub INSERT? Sending before is faster UX.
The stubs can be inserted asynchronously after the response.

---

### S3 — Queue embed display for incomplete stubs (artist = NULL)

**Verify:** What does the queue embed show for a playlist stub where
`artist_display = NULL` and `youtube_uploader = NULL`?

The queue entry format is: `{position}. {title} — {artist}`. With both
null, this becomes: `2. Never Gonna Give You Up — ` (trailing dash and
space). This looks broken.

Fix the queue embed render to handle null artist gracefully:

```rust
let artist_str = entry.artist
    .as_deref()
    .filter(|s| !s.is_empty())
    .unwrap_or("YouTube");  // sensible fallback before enrichment fills it in
```

---

### S4 — Duplicate video_id within same playlist

**Verify end-to-end:** Some YouTube playlists legitimately contain the
same video twice. The batch stub INSERT uses `ON CONFLICT (youtube_video_id)
DO NOTHING`. The second occurrence of the video_id produces no new row.
The RETURNING clause gives back `track_id` only for newly inserted rows.

**Check:** Does the implementation correctly look up the existing `track_id`
for the conflicted rows, or does it assume RETURNING gives back all rows
including conflicts? If it assumes all rows are returned, the queue will
have `QueueEntry` items with no `track_id` for the duplicate entries.

---

### S5 — Active download tasks not awaited on graceful shutdown

**Verify:** When the bot receives SIGTERM (typical for systemd or Docker
stop), what happens to in-progress download tasks? `tokio::spawn` tasks
are dropped when the runtime shuts down. With `kill_on_drop(true)` on
the child (per C4 fix), the yt-dlp processes are killed. But the DB
row may still show `status = 'downloading'`. On next startup, the
`unlock_stale_download_jobs` cleanup resets these rows (per the design
spec). Verify this cleanup runs correctly and that the timing threshold
(1 hour) is appropriate.

---

## Verification Checklist

```bash
# Type safety and compilation:
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# C1 — No signed CDN URL passed to YoutubeDl::new:
grep -rn "YoutubeDl::new\|YoutubeDl::new_ytdl_like" crates/adapters-discord/src/
# Verify all occurrences use watch?v= URLs or page_url, not stream_url/googlevideo

# C2 — ndjson parsing:
# yt-dlp --dump-json --flat-playlist {any_playlist_url} | head -3
# Verify first line is a valid JSON object (not the whole array)
# Run fetch_playlist_metadata in a test with known playlist

# C3 — uploader null for flat entries:
# Queue a playlist → check DB immediately:
# SELECT youtube_uploader, blob_location FROM tracks
#   WHERE source = 'youtube' ORDER BY created_at DESC LIMIT 10;
# → uploader = NULL is expected for fresh flat stubs
# After download completes:
# → uploader should be populated (repair step ran)

# C4 — kill_on_drop:
# Start a download, kill the bot process mid-download (kill -9)
# Check: yt-dlp processes no longer running (ps aux | grep yt-dlp)

# C5 — enrichment/analysis not picking up stubs:
# After queueing a YouTube playlist, wait 60 seconds
# SELECT enrichment_attempts, analysis_attempts
#   FROM tracks WHERE source='youtube' AND blob_location IS NULL;
# → Both should be 0 throughout

# M1 — batch insert timing:
# Queue a 50-track playlist, measure time between /play response and
# "Added 50 tracks" message. Should be < 5 seconds total.

# M2 — Shorts normalisation:
# /play https://www.youtube.com/shorts/dQw4w9WgXcQ
# /play https://www.youtube.com/watch?v=dQw4w9WgXcQ
# Both should resolve to the same track_id in the DB

# M3 — error classification:
# Trigger each error type, verify user sees the correct message

# M4 — timeout:
# Simulate network stall (iptables drop, disconnect NAS mid-download)
# Verify download times out after DOWNLOAD_TIMEOUT and retries

# O3 — startup validation:
# Remove yt-dlp binary temporarily
# Start bot → verify warning log, not panic
# Bot should still serve local library

# Full end-to-end:
# 1. /play {single_video} → streaming starts in ~3s, local file populated in ~30s
# 2. /play {same_url} → instant (<0.5s), plays from local file
# 3. /play {playlist_50_tracks} → all queued, only first 3 download immediately
# 4. Track advances → 4th track download starts automatically
# 5. After bot restart, /play {cached_url} → still instant from local file
```
