# TeamTI v2 — Pass 4 Implementation Prompt
## Atomic Tag Writeback & File–DB Synchronization

---

### Context

Passes 1–3 and their review passes are complete. The following are stable:

- Full scan pipeline: Watcher → Classifier → Fingerprint Worker (Pass 2)
- Full enrichment pipeline: AcoustID → MusicBrainz → Cover Art → `done` (Pass 3)
- Pass 3 deviation: `enrichment_status = 'done'` is set after Cover Art, without
  tag writeback. `// TODO(pass4)` comments mark every such call site.
- `SMB_READ_SEMAPHORE` is stored in `AppState` via `TypeMapKey` (Pass 2.1)

Pass 4 adds the final pipeline stage: writing enriched metadata back to the
audio file's ID3/Vorbis/MP4 tags using lofty. After this pass, files on the
NAS have accurate tags readable by any player — not just by this bot.

---

### Design Decision: `done` stays in Cover Art Worker

The master document places `done` after tag writeback. Pass 3 moved it earlier
as a deliberate deviation to make tracks immediately searchable. Pass 4 does
NOT revert this. The final architecture is:

```
Cover Art Worker → sets enrichment_status = 'done'   (immediate user visibility)
Tag Writer Worker → writes file tags, sets tags_written_at   (eventual file sync)
```

`done` means "enrichment complete and track is user-accessible."
`tags_written_at` is the new column that tracks file synchronization.

Remove all `// TODO(pass4)` comments from Pass 3. They are resolved by this pass.

---

### Acceptance Criteria

- [ ] `cargo build --workspace` — zero errors, zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] After enrichment completes, the audio file on the NAS has updated
      `TIT2` (MP3), `TITLE` (Vorbis), or `©nam` (M4A) matching `tracks.title`
- [ ] After tag writeback, `tracks.file_modified_at` and `tracks.file_size_bytes`
      reflect the new mtime and size from `stat()`
- [ ] `tracks.tags_written_at` is set to `now()` after successful writeback
- [ ] A `.tmp` temp file is never left orphaned on the NAS after a successful run
- [ ] If the process is killed mid-write, the original audio file is intact
      (temp file may be orphaned — that is acceptable)
- [ ] `tags_written_at` is reset to NULL whenever `update_enriched_metadata`
      is called (re-enrichment triggers re-writeback)
- [ ] Startup poller emits all `done` tracks with `tags_written_at IS NULL`
      to the Tag Writer channel within 10 seconds of startup
- [ ] The Tag Writer shares `SMB_READ_SEMAPHORE` with the Fingerprint Worker
      (concurrent SMB operations stay within semaphore limit)

---

### Scope

| Crate | Action |
|---|---|
| `crates/adapters-persistence/` | **Extend** — new migration, new repo methods |
| `crates/adapters-media-store/` | **Extend** — add `FileTagWriterPort` impl and tag write logic |
| `crates/application/` | **Extend** — add `ToTagWriter` event, `TagWriterWorker` |
| `apps/bot/` | **Extend** — fan-out from Cover Art Worker, startup poller, wire Tag Writer |

**Does NOT touch:** `adapters-acoustid`, `adapters-musicbrainz`, `adapters-cover-art`,
`adapters-discord`, `adapters-voice`.

---

### Channel Topology Addition

```
[existing: Cover Art Worker]
    │  (sets enrichment_status = 'done')
    │  ToTagWriter { track_id, blob_location }      cap: 128
    ▼
Tag Writer Worker  (application layer, single tokio task)
    │  fetch track + album metadata from DB  (async, before spawn_blocking)
    │  acquire SMB_READ_SEMAPHORE (owned)
    │  spawn_blocking:
    │    copy original → temp (.{name}.{uuid}.tmp)
    │    open temp with lofty, update tags, save_to_path(temp)
    │    std::fs::rename(temp, original)  ← atomic on POSIX / SMB
    │    stat(original) → new mtime, size_bytes
    │  UPDATE tracks SET file_modified_at, file_size_bytes,
    │                    tags_written_at = now()  (async)
    ▼
[file tags synchronized; track already 'done']
```

---

### Step 1 — New Migration
See if this step has already been incorporated first.
Create `crates/adapters-persistence/migrations/000x_tags_written_at.sql`:

```sql
ALTER TABLE tracks ADD COLUMN IF NOT EXISTS
    tags_written_at TIMESTAMPTZ DEFAULT NULL;

-- Index for the startup poller query
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_tracks_tags_unwritten
    ON tracks (id)
    WHERE enrichment_status = 'done' AND tags_written_at IS NULL;
```

The `CONCURRENTLY` keyword allows the index to be built without locking
the table. This is safe here because the migration runs before any tag writer
tasks start.

---

### Step 2 — `ToTagWriter` event in `application/src/events.rs`

Add to the existing events module:

```rust
/// Emitted by the Cover Art Worker after a track reaches 'done'.
/// Consumed by the Tag Writer Worker.
#[derive(Debug, Clone)]
pub struct ToTagWriter {
    pub track_id:      Uuid,
    /// Relative path to the audio file (relative to MEDIA_ROOT).
    /// Passed through to avoid a DB round trip in the worker.
    pub blob_location: String,
}
```

---

### Step 3 — `TagWriterWorker` in `application/src/tag_writer_worker.rs`

The worker is an async task that fetches metadata, acquires the SMB permit,
and delegates the blocking file work to `spawn_blocking`.

```rust
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore, OwnedSemaphorePermit};
use tracing::{info, warn};

use crate::AppError;
use crate::events::ToTagWriter;
use crate::ports::{FileTagWriterPort, TrackRepository, AlbumRepository};

pub struct TagWriterWorker {
    pub tag_writer:   Arc<dyn FileTagWriterPort>,
    pub track_repo:   Arc<dyn TrackRepository>,
    pub album_repo:   Arc<dyn AlbumRepository>,
    pub smb_semaphore: Arc<Semaphore>,
    pub media_root:   PathBuf,
}

impl TagWriterWorker {
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<ToTagWriter>,
    ) {
        while let Some(msg) = rx.recv().await {
            let worker = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = worker.process(msg).await {
                    warn!("tag_writer: error: {e}");
                }
            });
        }
    }

    async fn process(&self, msg: ToTagWriter) -> Result<(), AppError> {
        // Fetch full track + album data BEFORE acquiring the SMB permit.
        // DB reads are async and fast; don't hold the SMB permit during them.
        let track = self.track_repo
            .find_by_id(msg.track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {}", msg.track_id)))?;

        let album = match track.album_id {
            Some(album_id) => self.album_repo.find_by_id(album_id).await?,
            None => None,
        };

        let abs_path = self.media_root.join(&msg.blob_location);

        // Acquire SMB permit (owned) before any file operations.
        // Shared with Fingerprint Worker — caps total concurrent SMB ops.
        let smb_permit: OwnedSemaphorePermit = self.smb_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "SMB semaphore closed",
            )))?;

        // Prepare tag data — extract now while we're still async
        let tags = TagData {
            title:        track.title.clone(),
            artist:       track.artist_display.clone().unwrap_or_default(),
            album_title:  album.as_ref().map(|a| a.title.clone()),
            year:         track.year,
            genre:        track.genre.clone(),
            track_number: track.track_number.map(|n| n as u32),
            disc_number:  track.disc_number.map(|n| n as u32),
        };

        // spawn_blocking: copy → tag → rename → stat
        let result = tokio::task::spawn_blocking(move || {
            let _permit = smb_permit; // dropped when closure returns
            write_tags_atomic(&abs_path, &tags)
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("tag writer panic: {e}"),
        )))??;

        // Update DB with new mtime/size and set tags_written_at
        self.track_repo
            .update_file_tags_written(
                msg.track_id,
                result.new_mtime,
                result.new_size_bytes,
            )
            .await?;

        info!(
            "tag_writer: synced {} (mtime updated, {} bytes)",
            msg.blob_location,
            result.new_size_bytes
        );

        Ok(())
    }
}

/// Metadata passed into spawn_blocking.
/// All Clone + Send — safe to move into the blocking closure.
#[derive(Clone)]
pub struct TagData {
    pub title:        String,
    pub artist:       String,
    pub album_title:  Option<String>,
    pub year:         Option<i32>,
    pub genre:        Option<String>,
    pub track_number: Option<u32>,
    pub disc_number:  Option<u32>,
}

/// Result returned from the blocking write operation.
pub struct WriteResult {
    pub new_mtime:      chrono::DateTime<chrono::Utc>,
    pub new_size_bytes: i64,
}
```

---

### Step 4 — `write_tags_atomic` in `adapters-media-store/src/tag_writer.rs`

This function runs inside `spawn_blocking`. It holds the SMB permit for its
entire duration. It must never be called from an async context directly.

```rust
use std::path::Path;
use std::time::SystemTime;

use lofty::prelude::*;
use lofty::probe::Probe;
use chrono::DateTime;

use crate::tag_writer_worker::{TagData, WriteResult};

/// Writes tags atomically:
///   1. Copy original to .{name}.{uuid}.tmp (same directory = same filesystem)
///   2. Open temp file with lofty, update tags, save_to_path(temp)
///   3. std::fs::rename(temp, original) — atomic on POSIX and SMB
///   4. stat(original) → new mtime + size
///
/// If any step fails, the temp file is cleaned up and an error is returned.
/// The original file is NEVER modified directly.
pub fn write_tags_atomic(
    path: &Path,
    tags: &TagData,
) -> Result<WriteResult, Box<dyn std::error::Error + Send + Sync>> {
    let dir = path.parent()
        .ok_or("path has no parent directory")?;
    let filename = path.file_name()
        .ok_or("path has no filename")?
        .to_string_lossy();

    // Temp file: hidden dot-prefix + UUID suffix → invisible to scanner
    // Extension is not .mp3 etc. so Classifier skips it on any stray event
    let temp_path = dir.join(format!(
        ".{}.{}.tmp",
        filename,
        uuid::Uuid::new_v4()
    ));

    // Guard: ensure temp is removed on any error path
    let _temp_guard = TempGuard::new(&temp_path);

    // Step 1: Copy original → temp (preserves all audio data)
    std::fs::copy(path, &temp_path)?;

    // Step 2: Open temp with lofty, modify tags
    {
        let mut tagged = Probe::open(&temp_path)?.read()?;

        // Use the primary tag if present; otherwise use the first available tag.
        // If no tag exists, we cannot write — skip gracefully.
        let tag = tagged
            .primary_tag_mut()
            .or_else(|| tagged.first_tag_mut())
            .ok_or("no writable tag found in file")?;

        tag.set_title(tags.title.clone().into());
        tag.set_artist(tags.artist.clone().into());

        if let Some(ref album) = tags.album_title {
            tag.set_album(album.clone().into());
        }
        if let Some(year) = tags.year {
            tag.set_year(year as u32);
        }
        if let Some(ref genre) = tags.genre {
            tag.set_genre(genre.clone().into());
        }
        if let Some(track_num) = tags.track_number {
            tag.set_track(track_num);
        }
        if let Some(disc_num) = tags.disc_number {
            tag.set_disk(disc_num);
        }

        // Save modified tags to the TEMP file (not the original)
        tagged.save_to_path(&temp_path)?;
    }

    // Step 3: Atomic rename temp → original
    // On the same filesystem (same SMB share), rename() is atomic.
    // If rename fails (e.g. cross-device), return error — temp cleaned by guard.
    std::fs::rename(&temp_path, path)?;

    // Step 4: stat the original (now contains new tags)
    let meta = std::fs::metadata(path)?;
    let new_mtime: DateTime<chrono::Utc> = meta.modified()?.into();
    let new_size_bytes = meta.len() as i64;

    // Disarm the temp guard — rename succeeded, temp no longer exists
    _temp_guard.disarm();

    Ok(WriteResult { new_mtime, new_size_bytes })
}

/// RAII: removes the temp file on drop unless disarmed.
struct TempGuard {
    path:    std::path::PathBuf,
    armed:   bool,
}

impl TempGuard {
    fn new(path: &Path) -> Self {
        Self { path: path.to_owned(), armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
```

Add to `adapters-media-store/src/lib.rs`:
```rust
pub mod tag_writer;
pub use tag_writer::write_tags_atomic;
```

#### `FileTagWriterPort` implementation

```rust
// adapters-media-store/src/tag_writer_port.rs
use std::path::PathBuf;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use application::ports::FileTagWriterPort;
use application::AppError;
use application::tag_writer_worker::{TagData, WriteResult};

use crate::tag_writer::write_tags_atomic;

pub struct FileTagWriterAdapter {
    pub media_root:    PathBuf,
    pub smb_semaphore: Arc<Semaphore>,
}

#[async_trait]
impl FileTagWriterPort for FileTagWriterAdapter {
    async fn write_tags(
        &self,
        blob_location: &str,
        tags: &TagData,
    ) -> Result<WriteResult, AppError> {
        let abs_path  = self.media_root.join(blob_location);
        let tags_data = tags.clone();

        let permit: OwnedSemaphorePermit = self.smb_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "SMB semaphore closed",
            )))?;

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            write_tags_atomic(&abs_path, &tags_data)
                .map_err(|e| AppError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )))
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("spawn_blocking panic: {e}"),
        )))?
    }
}
```

---

### Step 5 — Persistence Methods

#### `update_file_tags_written` — new method on `TrackRepository` trait

```rust
/// Called after successful atomic tag writeback.
/// Updates file identity fields and sets tags_written_at = now().
async fn update_file_tags_written(
    &self,
    id:             Uuid,
    new_mtime:      chrono::DateTime<chrono::Utc>,
    new_size_bytes: i64,
) -> Result<(), AppError>;
```

Implementation:
```rust
sqlx::query!(
    r#"
    UPDATE tracks
    SET file_modified_at  = $2,
        file_size_bytes   = $3,
        tags_written_at   = now(),
        updated_at        = now()
    WHERE id = $1
    "#,
    id, new_mtime, new_size_bytes
)
.execute(&self.pool)
.await?;
Ok(())
```

#### `reset_tags_written_at` — called on re-enrichment

When `update_enriched_metadata` is called (track is re-enriched with new
MusicBrainz data), reset `tags_written_at` to NULL so the Tag Writer
re-synchronizes the file.

Add to the existing `update_enriched_metadata` SQL:
```sql
UPDATE tracks SET
    title                  = $2,
    artist_display         = $3,
    album_id               = $4,
    genre                  = $5,
    year                   = $6,
    mbid                   = $7,
    acoustid_id            = $8,
    enrichment_confidence  = $9,
    tags_written_at        = NULL,   -- ← reset: re-enrichment requires re-writeback
    updated_at             = now()
WHERE id = $1
```

#### `find_tags_unwritten` — startup poller query

```rust
async fn find_tags_unwritten(&self, limit: i64) -> Result<Vec<Track>, AppError>;
```

```sql
SELECT * FROM tracks
WHERE enrichment_status = 'done'
  AND tags_written_at IS NULL
ORDER BY updated_at ASC
LIMIT $1
```

---

### Step 6 — Fan-out from Cover Art Worker

In `application/src/cover_art_worker.rs`, the `process()` method currently
ends by setting `enrichment_status = 'done'`. Add the Tag Writer emit
immediately after:

```rust
async fn process(&self, msg: ToCoverArt) {
    // ... existing cover art resolution ...

    // Set enrichment_status = 'done' (Pass 3 deviation — now permanent)
    let _ = self.track_repo.update_enrichment_status(
        msg.track_id,
        &EnrichmentStatus::Done,
        0,
        Some(chrono::Utc::now()),
    ).await;

    // Fan-out to Tag Writer
    if let Some(ref tx) = self.tag_writer_tx {
        let _ = tx.send(ToTagWriter {
            track_id:      msg.track_id,
            blob_location: msg.blob_location.clone(),
        }).await;
    }

    info!("cover_art: track {} → done, queued for tag writeback", msg.track_id);
}
```

Add `tag_writer_tx: Option<mpsc::Sender<ToTagWriter>>` field to
`CoverArtWorker`. It is `None` until Pass 4 is wired in `apps/bot`.

Update `CoverArtWorker` struct:
```rust
pub struct CoverArtWorker {
    pub port:           Arc<dyn CoverArtPort>,
    pub track_repo:     Arc<dyn TrackRepository>,
    pub album_repo:     Arc<dyn AlbumRepository>,
    pub media_root:     PathBuf,
    pub tag_writer_tx:  Option<mpsc::Sender<ToTagWriter>>,  // ← new
}
```

---

### Step 7 — Startup Poller for Unwritten Tags

The startup poller handles tracks that reached `done` in Pass 3 but never had
their file tags written. It runs once at startup and then on a 2-hour interval.

Add `startup_tag_poller` to `application/src/tag_writer_worker.rs`:

```rust
pub async fn run_startup_tag_poller(
    track_repo:    Arc<dyn TrackRepository>,
    tag_writer_tx: mpsc::Sender<ToTagWriter>,
    poll_interval_secs: u64,
) {
    let mut interval = tokio::time::interval(
        std::time::Duration::from_secs(poll_interval_secs)
    );
    // First tick fires immediately — process the Pass 3 backlog on startup.
    loop {
        interval.tick().await;

        match track_repo.find_tags_unwritten(200).await {
            Ok(tracks) => {
                let count = tracks.len();
                if count > 0 {
                    tracing::info!("tag_poller: found {count} tracks pending writeback");
                }
                for track in tracks {
                    let _ = tag_writer_tx.send(ToTagWriter {
                        track_id:      track.id,
                        blob_location: track.blob_location,
                    }).await;
                }
            }
            Err(e) => {
                tracing::warn!("tag_poller: find_tags_unwritten error: {e}");
            }
        }
    }
}
```

---

### Step 8 — Wire in `apps/bot/main.rs`

```rust
use application::{
    TagWriterWorker,
    tag_writer_worker::run_startup_tag_poller,
    events::ToTagWriter,
};
use adapters_media_store::tag_writer_port::FileTagWriterAdapter;

// Retrieve SMB_READ_SEMAPHORE from AppState (stored by MediaScanner in Pass 2)
let smb_semaphore = {
    let data = client.data.read().await;
    Arc::clone(data.get::<SmbSemaphoreKey>().expect("SMB semaphore not in TypeMap"))
};

// Tag Writer channel
let (tag_writer_tx, tag_writer_rx) = mpsc::channel::<ToTagWriter>(128);

// FileTagWriterAdapter
let file_tag_writer = Arc::new(FileTagWriterAdapter {
    media_root:    config.media_root.clone(),
    smb_semaphore: Arc::clone(&smb_semaphore),
});

// Tag Writer Worker
{
    let worker = Arc::new(TagWriterWorker {
        tag_writer:    file_tag_writer,
        track_repo:    Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        album_repo:    Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
        smb_semaphore: Arc::clone(&smb_semaphore),
        media_root:    config.media_root.clone(),
    });
    let tok = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = worker.run(tag_writer_rx) => {}
        }
    });
}

// Startup poller for unwritten tags (handles Pass 3 backlog)
{
    let repo   = Arc::clone(&track_repo) as Arc<dyn TrackRepository>;
    let tx     = tag_writer_tx.clone();
    let tok    = token.clone();
    let secs   = config.scan_interval_secs * 4; // poll every ~20 min default
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = run_startup_tag_poller(repo, tx, secs) => {}
        }
    });
}

// Pass tag_writer_tx to CoverArtWorker (created earlier in the startup sequence)
// This requires CoverArtWorker to be built with tag_writer_tx populated.
// Update the CoverArtWorker construction block from Pass 3:
let cover_art_worker = Arc::new(CoverArtWorker {
    port:          cover_art_adapter,
    track_repo:    Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
    album_repo:    Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
    media_root:    config.media_root.clone(),
    tag_writer_tx: Some(tag_writer_tx),  // ← was None in Pass 3
});
```

Remove `// TODO(pass4)` comments from Cover Art Worker.

---

### Step 9 — `adapters-media-store/Cargo.toml` additions

```toml
uuid  = { workspace = true }         # for TempGuard UUID generation
lofty = "0.21"                       # already present; verify version
```

No new crates required — `lofty` is already a dependency of
`adapters-media-store` from Pass 2.1 (used for tag reading in `tag_reader.rs`).

---

### Step 10 — Integration Test

Add `crates/adapters-media-store/tests/tag_writer.rs`. Guard with
`TEST_DATABASE_URL` and a real audio file fixture.

```
test: write_tags_atomic on a copy of a real FLAC file
  → title, artist, album updated in file
  → mtime changes after write
  → original file content (audio data) unchanged (compare frame count)
  → no temp file left on disk after success

test: write_tags_atomic — process killed mid-copy simulation
  → create a read-only temp directory: write will fail at rename
  → assert: original file is unchanged
  → assert: temp file removed by TempGuard

test: full pipeline — track reaches tags_written_at IS NOT NULL
  → drop a real mp3 into MEDIA_ROOT (requires TEST_ACOUSTID_KEY)
  → wait for enrichment_status = 'done'
  → wait for tags_written_at IS NOT NULL (max 30s after done)
  → assert: file title tag matches tracks.title in DB
```

---

### Step 11 — Verify No `TODO(pass4)` Remains

After completing all steps, run:

```bash
grep -rn "TODO(pass4)" --include="*.rs" .
```

Expected output: **empty**. If any remain, they identify code paths that
still need to be updated to use the `tag_writer_tx`.

---

### Classifier Interaction After Tag Writeback

When the Tag Writer renames the temp file over the original, `PollWatcher`
will fire a `CreateOrModify` event for the modified file. The Classifier
will detect a mtime/size change and send `ToFingerprint`. The Fingerprint
Worker will compute the same Chromaprint fingerprint (audio data is unchanged)
and take the "same audio, same location" path, updating only
`file_modified_at` and `file_size_bytes`.

This is correct and harmless. However, the Tag Writer already updates these
fields via `update_file_tags_written`. The net effect is that `file_modified_at`
and `file_size_bytes` are written twice for each tag writeback — once by the
Tag Writer, once by the Fingerprint Worker catching the file change event.

To prevent the redundant Fingerprint Worker decode: the `update_file_tags_written`
call must complete **before** the next poll cycle fires. At the default
`SCAN_INTERVAL_SECS = 300s`, this is almost always guaranteed. No special
handling is needed — the Classifier will see the DB mtime matches the file
mtime on the next cycle and skip the file.

---

### Invariants (Pass 4 Specific)

| Rule | Detail |
|---|---|
| `SMB_READ_SEMAPHORE` shared, not duplicated | Retrieve from AppState TypeMap; never construct a new Semaphore for the tag writer |
| Temp file is always in the same directory as the original | Different filesystem = rename fails with EXDEV; never use `/tmp` |
| Temp file name starts with `.` and ends with `.tmp` | Prevents Classifier pickup on any stray watcher event |
| `TempGuard` must be a named `let` binding, never anonymous | Same drop-ordering rule as `ScanGuard` in Pass 2.1 |
| `lofty::save_to_path` is called on the TEMP file, not the original | Direct in-place write is not atomic; always write to temp then rename |
| `tags_written_at` is reset to NULL in `update_enriched_metadata` | Ensures re-enrichment always triggers a fresh tag writeback |

---

### REFERENCE

docs/v2/v2_master.md