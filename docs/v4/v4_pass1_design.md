# TeamTI v4 — Pass 1 Design Spec
## YouTube Playback: Hybrid Stream-While-Cache Architecture

> All decisions locked. Attach alongside all v3 output files,
> the full migrations directory, `state.rs`, `lifecycle_worker.rs`,
> the enrichment + analysis worker files, and the `/play` command
> before sending to the agent.

---

## Decision Log

| Topic | Decision |
|-------|----------|
| Architecture | Hybrid: stream immediately (Approach A) + download in background (Approach B) |
| Storage path | `$MEDIA_ROOT/youtube/{uploader}/{album_if_exists}/{title}_{video_id}.opus` |
| UX transparency | Completely transparent — no "streaming" vs "cached" indicator ever shown |
| NP message | Unchanged — same auto-post flow as local tracks |
| Cache eviction | None (manual only) — tracks persist indefinitely |
| URL scope | Single video, Shorts, playlists (unlimited queue, JIT download), search fallback |
| Playlist queue | Queue all entries silently; JIT download for lookahead only |
| Download strategy | Just-in-time: always download next `YTDLP_LOOKAHEAD_DEPTH` undownloaded tracks |
| Search fallback | `/play {text}` → local library first; if empty → YouTube top-1 auto-queued |
| Bot detection | Residential NAS; `YTDLP_COOKIES_FILE` env var supported |
| Stub cleanup | Delete stub tracks with no listen history on permanent failure; keep stubs with history |
| Concurrency model | `tokio::Semaphore` for download bounding; `tokio::process::Command` for yt-dlp |

---

## Execution Flow — Complete Decision Tree

```
/play {input}
  │
  ├─ Is input a YouTube URL? (youtube.com, youtu.be, music.youtube.com)
  │    │
  │    ├─ YES: Is it a playlist URL? (playlist?list= or /playlist/)
  │    │         │
  │    │         ├─ YES → Playlist flow (§Playlist Flow)
  │    │         └─ NO  → Single video flow (§Single Video Flow)
  │    │
  │    └─ NO: Plain text → search local library
  │              │
  │              ├─ Local results found → existing autocomplete behaviour
  │              └─ No local results → YouTube search flow (§Search Flow)

§Single Video Flow:
  1. Extract video_id from URL
  2. Look up youtube_download_jobs WHERE video_id = $id:
     │
     ├─ status = 'done'
     │    → Queue Track from blob_location (instant, <0.5s)
     │    → trigger_lookahead(guild_id)
     │    → respond "Added to queue: {title}"
     │
     ├─ status IN ('pending', 'downloading')
     │    → Stub Track already exists (concurrent request or crash recovery)
     │    → Check stream_url_expires_at; if expired → re-fetch metadata (1-3s)
     │    → Queue via Songbird YoutubeDl streaming input
     │    → trigger_lookahead(guild_id)
     │    → respond "Added to queue: {title}"
     │
     ├─ status = 'failed' (attempts < MAX_DOWNLOAD_ATTEMPTS)
     │    → Same as 'pending' — stream while retrying download
     │
     ├─ status = 'permanently_failed'
     │    → ephemeral error: "This video is no longer available."
     │
     └─ No record exists (first time):
          → yt-dlp --dump-json (1-3s): get title, uploader, duration, stream_url
          → Create stub Track record (blob_location = NULL)
          → Insert youtube_download_jobs (status='pending', stream_url, stream_url_expires_at)
          → Queue via Songbird YoutubeDl streaming input
          → trigger_lookahead(guild_id)
          → respond "Added to queue: {title}"

§Playlist Flow:
  1. yt-dlp --dump-json --flat-playlist {url} (1-3s, returns all entries)
  2. For each entry in order:
     → cache check (same decision tree as §Single Video Flow)
     → queue accordingly (local file OR streaming)
  3. trigger_lookahead(guild_id)
  4. respond "Added {n} tracks to queue."

§Search Flow:
  1. yt-dlp --dump-json "ytsearch1:{query}" (2-4s)
  2. Process top result through §Single Video Flow

§trigger_lookahead(guild_id):
  1. Get guild's queue_meta positions [1 .. YTDLP_LOOKAHEAD_DEPTH]
  2. For each position with source = QueueSource::YouTube AND blob_location IS NULL:
     → schedule_download(video_id, url, output_path) via YoutubeDownloadWorker
     (bounded by Semaphore — respects YTDLP_DOWNLOAD_CONCURRENCY)
```

---

## Schema Changes — Migration `0007_youtube.sql`

```sql
-- ── YouTube metadata columns on tracks ───────────────────────────────────
-- All nullable — NULL for local tracks, populated for YouTube-sourced tracks.
ALTER TABLE tracks
    ADD COLUMN IF NOT EXISTS source             TEXT NOT NULL DEFAULT 'local'
        CHECK (source IN ('local', 'youtube')),
    ADD COLUMN IF NOT EXISTS youtube_video_id   TEXT,
    ADD COLUMN IF NOT EXISTS youtube_channel_id TEXT,
    ADD COLUMN IF NOT EXISTS youtube_uploader   TEXT,
    ADD COLUMN IF NOT EXISTS youtube_thumbnail_url TEXT;

-- Unique index: prevents duplicate stubs for the same video.
-- Partial index: only on rows where youtube_video_id IS NOT NULL.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_youtube_video_id
    ON tracks(youtube_video_id)
    WHERE youtube_video_id IS NOT NULL;

-- ── YouTube download job queue ────────────────────────────────────────────
-- One row per unique video_id. Tracks the lifecycle of the background download.
-- status:
--   'pending'           → waiting for a download slot
--   'downloading'       → yt-dlp subprocess is running
--   'done'              → file written, blob_location set on tracks row
--   'failed'            → this attempt failed; will retry up to max_attempts
--   'permanently_failed'→ max attempts exhausted; stub may be cleaned up
CREATE TABLE IF NOT EXISTS youtube_download_jobs (
    video_id              TEXT PRIMARY KEY,
    track_id              UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    url                   TEXT NOT NULL,
    status                TEXT NOT NULL DEFAULT 'pending'
                              CHECK (status IN (
                                  'pending','downloading','done',
                                  'failed','permanently_failed'
                              )),
    attempts              INTEGER NOT NULL DEFAULT 0,
    error_message         TEXT,
    -- Signed stream URL from yt-dlp --dump-json. Used for Songbird streaming.
    -- Expires approximately 6 hours after metadata fetch.
    stream_url            TEXT,
    stream_url_expires_at TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at            TIMESTAMPTZ,
    completed_at          TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_youtube_download_jobs_status
    ON youtube_download_jobs(status, attempts)
    WHERE status IN ('pending', 'failed');
```

---

## Enrichment and Analysis Queue Fix

**Both workers must be gated on `blob_location IS NOT NULL`.**

YouTube stubs are created with `enrichment_status = 'pending'` and
`analysis_status = 'pending'` but `blob_location = NULL`. Without this
fix, both workers pick up the stub, try to open a non-existent file,
mark it `failed`, and increment `attempts` until it hits the retry cap
— corrupting the enrichment/analysis state for a track that hasn't
even finished downloading yet.

**In `adapters-persistence`, update both worker poll queries:**

```sql
-- Enrichment worker poll (existing query — add blob_location guard):
SELECT id, blob_location, title, ...
FROM tracks
WHERE enrichment_status IN ('pending', 'failed')
  AND enrichment_locked = false
  AND blob_location IS NOT NULL        -- ← ADD THIS
ORDER BY enrichment_attempts ASC, created_at ASC
LIMIT $batch_size
FOR UPDATE SKIP LOCKED

-- Analysis worker poll (existing query — add blob_location guard):
SELECT id, blob_location
FROM tracks
WHERE analysis_status IN ('pending', 'failed')
  AND analysis_locked = false
  AND blob_location IS NOT NULL        -- ← ADD THIS
ORDER BY analysis_attempts ASC, created_at ASC
LIMIT $batch_size
FOR UPDATE SKIP LOCKED
```

---

## New Crate: `crates/adapters-ytdlp`

```
crates/adapters-ytdlp/
├── Cargo.toml
└── src/
    ├── lib.rs          YtDlpAdapter: implements YtDlpPort
    ├── metadata.rs     Deserialization of yt-dlp --dump-json JSON output
    └── sanitize.rs     Filename/path sanitization for blob_location
```

### Cargo.toml

```toml
[package]
name    = "adapters-ytdlp"
version = "0.1.0"
edition = "2021"

[dependencies]
application  = { path = "../application" }
domain       = { path = "../domain" }
tokio        = { workspace = true, features = ["process", "fs"] }
serde        = { workspace = true, features = ["derive"] }
serde_json   = { workspace = true }
tracing      = { workspace = true }
thiserror    = { workspace = true }
async-trait  = { workspace = true }
chrono       = { workspace = true }
```

No yt-dlp Rust crate is used — yt-dlp is invoked directly as a
subprocess via `tokio::process::Command`. This avoids crate versioning
lag behind yt-dlp releases and gives full control over arguments.

### Core yt-dlp invocations

```rust
// ── Single video metadata ─────────────────────────────────────────────────
// Fast (~1-3s). Returns one JSON object with full format list + stream URLs.
yt-dlp \
    [--cookies {YTDLP_COOKIES_FILE}]  // optional
    --dump-json                        // print metadata JSON to stdout
    --no-playlist                      // treat playlist URLs as single video
    --no-warnings                      // suppress stderr noise
    "{url}"

// ── Playlist metadata (flat) ──────────────────────────────────────────────
// Fast (1-3s for any size). Each entry is a minimal JSON object on its own
// line (ndjson). Contains: id, url, title, duration — no format data.
yt-dlp \
    [--cookies {YTDLP_COOKIES_FILE}]
    --dump-json
    --flat-playlist                    // don't fetch format info per entry
    --no-warnings
    "{url}"

// ── YouTube search ────────────────────────────────────────────────────────
// Returns top N results. "ytsearch1:" returns exactly 1 result.
yt-dlp \
    [--cookies {YTDLP_COOKIES_FILE}]
    --dump-json
    --no-playlist
    --no-warnings
    "ytsearch1:{query}"

// ── Audio download ────────────────────────────────────────────────────────
// Downloads best available audio as Opus in OGG container.
// -f selector: prefer native Opus streams; fall back to any audio.
// --no-playlist: safety guard (don't accidentally download a whole playlist).
yt-dlp \
    [--cookies {YTDLP_COOKIES_FILE}]
    -f "bestaudio[ext=opus]/bestaudio[ext=webm]/bestaudio" \
    --extract-audio                    // strip video track
    --audio-format opus                // re-encode to .opus if not already
    --audio-quality 0                  // best quality
    --no-playlist \
    --no-warnings \
    -o "{output_path}" \               // absolute path on NAS
    "{url}"
```

### `VideoMetadata` struct (in `crates/domain`)

```rust
/// Metadata extracted from yt-dlp --dump-json output.
#[derive(Debug, Clone)]
pub struct VideoMetadata {
    pub video_id:              String,
    pub url:                   String,
    pub title:                 String,
    pub uploader:              String,        // YouTube channel name
    pub channel_id:            Option<String>,
    pub duration_ms:           Option<i32>,   // from "duration" (seconds) × 1000
    pub thumbnail_url:         Option<String>,
    // Sometimes populated for music videos uploaded by labels:
    pub track_title:           Option<String>, // "track" field in yt-dlp JSON
    pub artist:                Option<String>, // "artist" field
    pub album:                 Option<String>, // "album" field
    // Signed stream URL (expires ~6h after extraction):
    pub stream_url:            Option<String>,
    pub stream_url_expires_at: Option<DateTime<Utc>>,
}
```

### Blob path construction (in `sanitize.rs`)

```rust
/// Build the relative blob_location path for a YouTube download.
/// Format: youtube/{uploader}/{album_if_exists}/{title}_{video_id}.opus
///
/// video_id is appended to the filename to guarantee uniqueness even
/// when two videos have identical titles.
pub fn youtube_blob_path(meta: &VideoMetadata) -> String {
    let uploader = sanitize_component(&meta.uploader);
    let filename  = sanitize_component(
        &meta.track_title.as_deref().unwrap_or(&meta.title)
    );
    let video_id  = &meta.video_id;

    match &meta.album {
        Some(album) => {
            let album = sanitize_component(album);
            format!("youtube/{uploader}/{album}/{filename}_{video_id}.opus")
        }
        None => format!("youtube/{uploader}/{filename}_{video_id}.opus"),
    }
}

/// Sanitize a path component:
/// - Remove filesystem-unsafe chars: / \ : * ? " < > | NUL
/// - Collapse runs of whitespace to single space
/// - Trim leading/trailing whitespace and dots
/// - Truncate to MAX_COMPONENT_LEN (200) characters
/// - Never empty: fall back to "Unknown" if result is blank
fn sanitize_component(s: &str) -> String {
    const UNSAFE: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];
    let sanitized: String = s
        .chars()
        .map(|c| if UNSAFE.contains(&c) { '_' } else { c })
        .collect();
    let trimmed = sanitized.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "Unknown".to_string()
    } else {
        collapsed.chars().take(200).collect()
    }
}
```

---

## New Application Port: `YtDlpPort`

In `crates/application/src/ports/ytdlp.rs`:

```rust
#[async_trait]
pub trait YtDlpPort: Send + Sync {
    /// Fetch metadata for a single video (no download).
    /// Provides stream URL valid for ~6 hours.
    async fn fetch_video_metadata(
        &self,
        url: &str,
    ) -> Result<VideoMetadata, AppError>;

    /// Fetch flat playlist metadata (no per-video format fetch).
    /// Returns one entry per playlist item — fast for any playlist size.
    async fn fetch_playlist_metadata(
        &self,
        url: &str,
    ) -> Result<Vec<VideoMetadata>, AppError>;

    /// Search YouTube and return the top result's metadata.
    async fn search_top_result(
        &self,
        query: &str,
    ) -> Result<Option<VideoMetadata>, AppError>;

    /// Download audio for a single video to the given absolute path.
    /// Returns when the download is complete and the file is written.
    /// Long-running: call from a spawn_blocking or via tokio::process.
    async fn download_audio(
        &self,
        url:         &str,
        output_path: &std::path::Path,
    ) -> Result<(), AppError>;
}
```

---

## New Application Port: `YoutubeRepository`

In `crates/application/src/ports/repository.rs` (new methods):

```rust
// On YoutubeRepository or extend TrackRepository:

/// Insert stub Track for a YouTube video (blob_location = NULL).
/// Returns the new track_id.
async fn create_youtube_stub(
    &self,
    meta: &VideoMetadata,
) -> Result<Uuid, AppError>;

/// Look up an existing download job by video_id.
async fn get_download_job(
    &self,
    video_id: &str,
) -> Result<Option<YoutubeDownloadJob>, AppError>;

/// Insert a new download job (ON CONFLICT DO NOTHING — idempotent).
async fn upsert_download_job(
    &self,
    job: &NewYoutubeDownloadJob,
) -> Result<(), AppError>;

/// Mark job as 'downloading', set started_at.
async fn lock_download_job(
    &self,
    video_id: &str,
) -> Result<(), AppError>;

/// Mark job as 'done', set blob_location on associated track.
async fn complete_download_job(
    &self,
    video_id:      &str,
    blob_location: &str,
) -> Result<(), AppError>;

/// Mark job as 'failed', increment attempts, record error.
async fn fail_download_job(
    &self,
    video_id: &str,
    error:    &str,
) -> Result<(), AppError>;

/// Permanently fail job + optionally delete the stub track.
/// Deletes the stub only if it has zero listen_events.
async fn permanently_fail_download_job(
    &self,
    video_id: &str,
) -> Result<(), AppError>;

/// On startup: reset stuck 'downloading' jobs older than threshold.
async fn unlock_stale_download_jobs(
    &self,
    older_than: Duration,
) -> Result<u64, AppError>;
```

---

## New Worker: `YoutubeDownloadWorker`

In `crates/application/src/youtube_worker.rs`.

```rust
pub struct YoutubeDownloadWorker {
    ytdlp:       Arc<dyn YtDlpPort>,
    repo:        Arc<dyn YoutubeRepository>,
    semaphore:   Arc<tokio::sync::Semaphore>,   // bounded by YTDLP_DOWNLOAD_CONCURRENCY
    media_root:  PathBuf,
    max_attempts: u32,
}

impl YoutubeDownloadWorker {
    /// Schedule a download for video_id.
    /// Returns immediately — the download runs in a background task.
    /// Idempotent: calling for a video_id already downloading is a no-op.
    pub fn schedule(&self, video_id: String, url: String, blob_path: String) {
        let worker = self.clone();
        tokio::spawn(async move {
            worker.run_download(video_id, url, blob_path).await;
        });
    }

    async fn run_download(&self, video_id: String, url: String, blob_path: String) {
        // 1. Acquire semaphore permit (blocks if at concurrency limit)
        let _permit = self.semaphore.acquire().await;

        // 2. Check if still needed (another task may have raced and completed it)
        let job = self.repo.get_download_job(&video_id).await;
        if matches!(job, Ok(Some(ref j)) if j.status == "done") {
            return;
        }

        // 3. Mark as 'downloading'
        if let Err(e) = self.repo.lock_download_job(&video_id).await {
            tracing::warn!(video_id, error = %e, "failed to lock download job");
            return;
        }

        // 4. Construct absolute output path
        let output_path = self.media_root.join(&blob_path);
        if let Some(parent) = output_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        // 5. Run yt-dlp download
        match self.ytdlp.download_audio(&url, &output_path).await {
            Ok(()) => {
                // 6a. Success: update DB
                if let Err(e) = self.repo.complete_download_job(&video_id, &blob_path).await {
                    tracing::error!(video_id, error = %e, "download complete but DB update failed");
                } else {
                    tracing::info!(video_id, blob_path, "youtube download complete");
                    // Enrichment and analysis workers will pick this up automatically
                    // now that blob_location IS NOT NULL.
                }
            }
            Err(e) => {
                tracing::warn!(video_id, error = %e, "youtube download failed");
                let job = self.repo.get_download_job(&video_id).await.ok().flatten();
                let attempts = job.map(|j| j.attempts).unwrap_or(0);

                if attempts + 1 >= self.max_attempts as i32 {
                    // 6b. Permanently fail
                    tracing::error!(video_id, "youtube download permanently failed");
                    let _ = self.repo.permanently_fail_download_job(&video_id).await;
                } else {
                    // 6c. Mark failed for retry
                    let _ = self.repo.fail_download_job(&video_id, &e.to_string()).await;
                }
            }
        }
    }
}
```

---

## Changes to `/play` Command

In `crates/adapters-discord/src/commands/play.rs`:

### URL detection

```rust
fn classify_input(input: &str) -> PlayInput {
    let lower = input.to_lowercase();
    if lower.starts_with("https://www.youtube.com/")
        || lower.starts_with("https://youtube.com/")
        || lower.starts_with("https://youtu.be/")
        || lower.starts_with("https://music.youtube.com/")
    {
        if lower.contains("list=") || lower.contains("/playlist") {
            PlayInput::YoutubePlaylist(input.to_string())
        } else {
            PlayInput::YoutubeVideo(input.to_string())
        }
    } else if lower.starts_with("https://") || lower.starts_with("http://") {
        PlayInput::UnsupportedUrl
    } else {
        PlayInput::SearchQuery(input.to_string())
    }
}

enum PlayInput {
    YoutubeVideo(String),
    YoutubePlaylist(String),
    SearchQuery(String),
    UnsupportedUrl,
}
```

### Autocomplete handler

```rust
// On each autocomplete keypress for the `query` option:
async fn autocomplete_query(ctx: &Context, partial: &str) -> Vec<AutocompleteChoice> {
    // 1. Always try local library first
    let local_results = search_local_library(partial, 25).await;
    if !local_results.is_empty() {
        return local_results;
    }

    // 2. If input looks like a YouTube URL, return a single choice
    //    showing the URL itself (will be resolved on submission)
    if classify_input(partial) != PlayInput::SearchQuery(_) {
        return vec![AutocompleteChoice::new(partial, partial)];
    }

    // 3. Empty local results + plain text = YouTube search
    // NOTE: YouTube search in autocomplete is intentionally NOT done here —
    // autocomplete runs on every keystroke and yt-dlp takes 2-4s.
    // Instead, show a hint choice:
    if partial.len() >= 3 {
        return vec![AutocompleteChoice::new(
            format!("🔍 Search YouTube for: \"{}\"", partial),
            format!("__ytsearch__{}", partial),  // sentinel prefix handled on submit
        )];
    }

    vec![]
}
```

### Submission handler (simplified)

```rust
async fn handle_play(ctx: &Context, input: &str) -> Result<(), AppError> {
    ctx.defer().await?;

    match classify_input(input) {
        PlayInput::YoutubeVideo(url) => {
            handle_youtube_single(ctx, &url).await
        }
        PlayInput::YoutubePlaylist(url) => {
            handle_youtube_playlist(ctx, &url).await
        }
        PlayInput::SearchQuery(q) if q.starts_with("__ytsearch__") => {
            let query = q.trim_start_matches("__ytsearch__");
            handle_youtube_search(ctx, query).await
        }
        PlayInput::SearchQuery(q) => {
            // Normal local track play (existing behaviour)
            handle_local_play(ctx, &q).await
        }
        PlayInput::UnsupportedUrl => {
            ctx.followup_ephemeral("Only YouTube URLs are supported.").await
        }
    }
}
```

---

## `QueueSource` Extension

In `crates/adapters-voice/src/state.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueSource {
    Manual,
    Radio,
    YouTube,   // YouTube-sourced track (streaming or cached — transparent to display)
}
```

YouTube tracks show a 📺 prefix in the queue embed (similar to 🎲 for radio).

---

## Songbird Input Routing

The playback path depends on whether the track is cached:

```rust
// In the queue enqueue logic:
let input: songbird::input::Input = match (source, blob_location) {
    (QueueSource::YouTube, Some(path)) => {
        // Cached: play from local file — same as NAS tracks
        let abs_path = media_root.join(&path);
        songbird::input::File::new(abs_path).into()
    }
    (QueueSource::YouTube, None) => {
        // Streaming: use Songbird's built-in YoutubeDl
        // stream_url is the signed URL from yt-dlp --dump-json
        songbird::input::YoutubeDl::new(
            reqwest_client.clone(),
            stream_url,
        ).into()
    }
    (_, Some(path)) => {
        // Local NAS track
        let abs_path = media_root.join(&path);
        songbird::input::File::new(abs_path).into()
    }
    (_, None) => {
        return Err(AppError::Playback("Track has no audio file.".into()));
    }
};
```

---

## Lookahead Trigger Integration

In `crates/adapters-discord/src/lifecycle_worker.rs`,
in the `TrackStarted` event handler:

```rust
// After posting the NP embed and starting the update task:
youtube_download_worker.trigger_lookahead(guild_id, &queue_meta).await;
```

`trigger_lookahead` inspects positions `[1..=YTDLP_LOOKAHEAD_DEPTH]`
of `queue_meta` for entries with `source == QueueSource::YouTube` and
`blob_location == None`, then calls `schedule()` on each.

Also call `trigger_lookahead` after:
- Any tracks are added to the queue (`/play`, `/playlist play`)
- `/queue shuffle` (order changed, different tracks now in lookahead window)

---

## Startup Cleanup

In `apps/bot/src/main.rs`, before spawning workers:

```rust
// Reset stuck download jobs (crashed mid-download):
let unlocked = youtube_repo
    .unlock_stale_download_jobs(Duration::from_secs(3600))
    .await
    .expect("failed to unlock stale youtube download jobs");

if unlocked > 0 {
    tracing::warn!(
        count = unlocked,
        "reset stale youtube download jobs from previous session"
    );
}
```

---

## Config Additions (`shared-config`)

```rust
/// Absolute path to the yt-dlp binary.
pub ytdlp_binary: String,          // env: YTDLP_BINARY, default: "yt-dlp" (uses $PATH)

/// Optional: path to Netscape-format cookies file for bot detection bypass.
pub ytdlp_cookies_file: Option<String>,  // env: YTDLP_COOKIES_FILE

/// Max simultaneous yt-dlp download processes.
pub ytdlp_download_concurrency: usize,   // env: YTDLP_DOWNLOAD_CONCURRENCY, default: 2

/// Number of tracks ahead in queue to pre-download (just-in-time).
pub ytdlp_lookahead_depth: usize,        // env: YTDLP_LOOKAHEAD_DEPTH, default: 3

/// Max download attempts before permanent failure.
pub ytdlp_max_download_attempts: u32,    // env: YTDLP_MAX_DOWNLOAD_ATTEMPTS, default: 5
```

`.env.example` additions:
```
# yt-dlp integration (required for YouTube playback)
YTDLP_BINARY=yt-dlp
YTDLP_COOKIES_FILE=          # optional: /path/to/cookies.txt
YTDLP_DOWNLOAD_CONCURRENCY=2
YTDLP_LOOKAHEAD_DEPTH=3
YTDLP_MAX_DOWNLOAD_ATTEMPTS=5
```

---

## Permanent Failure + Stub Cleanup SQL

In `adapters-persistence` — `permanently_fail_download_job`:

```sql
-- Step 1: Update job status
UPDATE youtube_download_jobs
SET status = 'permanently_failed'
WHERE video_id = $1;

-- Step 2: Delete stub track IF it has no listen_events
-- (leaves tracks with listen history intact — they have sentimental/recommendation value)
DELETE FROM tracks
WHERE youtube_video_id = $1
  AND blob_location IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM listen_events WHERE track_id = tracks.id
  );
```

---

## Crates Affected

| Crate | Change |
|-------|--------|
| `crates/domain` | New types: `VideoMetadata`, `YoutubeDownloadJob`, `QueueSource::YouTube` |
| `crates/application` | New port `YtDlpPort`; new port methods on `YoutubeRepository`; new worker `YoutubeDownloadWorker`; enrichment/analysis query fix (add `blob_location IS NOT NULL` guard) |
| `crates/adapters-ytdlp` | **New crate** — yt-dlp subprocess wrapper, metadata parser, path sanitizer |
| `crates/adapters-persistence` | New `YoutubeRepository` impl; updated enrichment/analysis poll queries |
| `crates/adapters-discord` | Updated `/play` command: URL detection, YouTube flow, search fallback, playlist handling, autocomplete |
| `crates/adapters-voice` | `QueueSource::YouTube` variant; dual input routing (local file vs YoutubeDl stream) |
| `apps/bot` | New adapter instantiation, startup cleanup, channel wiring |
| `migrations/` | New `0007_youtube.sql` |

---

## Verification Plan

```bash
# Schema:
cargo sqlx migrate run
psql -c "\d tracks" | grep youtube
psql -c "\d youtube_download_jobs"
cargo sqlx prepare --workspace
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# Manual — single video first play (streaming):
# /play https://www.youtube.com/watch?v=dQw4w9WgXcQ
# Expected: responds "Added to queue: Never Gonna Give You Up" (~2-4s)
# Bot plays immediately (streaming from YouTube)
# SELECT blob_location, youtube_video_id FROM tracks WHERE youtube_video_id='dQw4w9WgXcQ';
# → blob_location = NULL initially, then populates after background download

# Manual — same video second play (cached):
# /play https://www.youtube.com/watch?v=dQw4w9WgXcQ
# Expected: responds near-instantly (< 0.5s)
# SELECT blob_location FROM tracks WHERE youtube_video_id='dQw4w9WgXcQ';
# → blob_location should be populated now

# Manual — JIT lookahead:
# /play {playlist_url} (3+ tracks)
# SELECT status, video_id FROM youtube_download_jobs ORDER BY created_at;
# Expected: position 1-3 in queue have status='downloading' or 'done'
# Remaining tracks have status='pending'

# Manual — search fallback:
# /play some obscure artist not in local library
# Expected: YouTube top result queued automatically

# Manual — enrichment not touching stubs:
# Confirm enrichment_attempts stays 0 for tracks with blob_location = NULL
# SELECT id, enrichment_attempts FROM tracks WHERE blob_location IS NULL AND source='youtube';

# Manual — permanent failure cleanup:
# Manually set attempts = 5 for a download job, trigger fail
# Confirm stub with no listen_events is deleted from tracks
# Confirm stub WITH listen_events is kept (blob_location = NULL)
```
