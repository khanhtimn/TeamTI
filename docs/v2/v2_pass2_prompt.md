# TeamTI v2 — Pass 2 Implementation Prompt
## Scan Pipeline: Watcher → Classifier → Fingerprint Worker → Enrichment Orchestrator

---

### Context

Pass 1 is complete. The following are now in place and must not be changed
unless a compile error requires it:

- `crates/domain/` — all v2 entities and `EnrichmentStatus`
- `crates/application/` — all port traits including `FingerprintPort`,
  `FileTagWriterPort`, `AcoustIdPort`, `MusicBrainzPort`, `CoverArtPort`
- `crates/shared-config/` — full `Config` struct with all v2 fields
- `crates/adapters-persistence/` — migrations 0001–0004 applied; partial
  `TrackRepository` impl with `insert`, `find_by_id`, `find_by_blob_location`,
  `find_by_fingerprint`, `mark_file_missing`, `reset_stale_enriching`,
  `force_rescan` implemented; remaining methods are `todo!()`

Pass 2 implements the **complete scan-side pipeline**, from filesystem event
detection through to the enrichment queue boundary. It does not implement any
external HTTP calls (AcoustID, MusicBrainz, Cover Art) — those are Pass 3.

---

### Acceptance Criteria

Pass 2 is complete when ALL of the following are true:

- [ ] `cargo build --workspace` — zero errors, zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] Dropping a supported audio file (mp3, flac, ogg, wav, aac, m4a, opus)
      into `MEDIA_ROOT` causes a new row to appear in `tracks` within
      `SCAN_INTERVAL_SECS + 10` seconds, with `enrichment_status = 'pending'`
- [ ] Modifying the mtime of an already-indexed file triggers a Fingerprint
      Worker run; if Chromaprint matches, only `file_modified_at` and
      `file_size_bytes` are updated; `enrichment_status` is unchanged
- [ ] Moving or renaming an indexed file updates `blob_location` only; track
      identity (UUID, fingerprint, enrichment status) is fully preserved
- [ ] Deleting an indexed file sets `enrichment_status = 'file_missing'`
- [ ] Dropping a duplicate file (same audio content, different path) logs a
      warning and does not insert a second row
- [ ] The PollWatcher never starts a new scan while the previous scan is
      still in progress (`scan_in_progress` AtomicBool guard verified by test)
- [ ] The `claim_for_enrichment` query uses `FOR UPDATE SKIP LOCKED` and
      returns only rows matching the retry matrix from the master document
- [ ] The Enrichment Orchestrator polls the DB, claims pending tracks, and
      emits track IDs to the AcoustID channel (consumed by a no-op logger in
      this pass); enrichment_status is set to 'enriching' on claim

---

### Scope

| Crate | Action |
|---|---|
| `crates/adapters-watcher/` | **Create** — new crate |
| `crates/adapters-media-store/` | **Extend** — add Classifier + Fingerprint Worker + FingerprintPort impl |
| `crates/adapters-persistence/` | **Complete** — implement stubbed methods needed by this pass |
| `crates/application/` | **Extend** — add EnrichmentOrchestrator use case |
| `apps/bot/` | **Extend** — wire full scan pipeline; add no-op AcoustID consumer |

### Does NOT touch

- `crates/adapters-discord/` — no changes
- `crates/adapters-voice/` — no changes
- `crates/adapters-acoustid/`, `adapters-musicbrainz/`, `adapters-cover-art/`
  — these crates do not exist yet; do not create them in this pass
- Discord slash commands — no behavioral changes
- Tag writeback — no lofty write calls in this pass (read-only)

---

### Channel Topology (This Pass)

```
PollWatcher (std::thread)
    │ FileEvent { path, kind }   [capacity: 2048]
    │ tx.blocking_send()
    ▼
Classifier (single tokio task)
    │ ToFingerprint { path, mtime, size_bytes, existing_id }   [capacity: 256]
    ▼
Fingerprint Worker pool (spawn_blocking, ≤FINGERPRINT_CONCURRENCY)
    │ ToEnrichment { track_id }   [capacity: 128]
    ▼
Enrichment Orchestrator (single tokio task)
    │ also polls DB every SCAN_INTERVAL_SECS independently
    │ ToAcoustId { track_id, fingerprint, duration_secs }   [capacity: 64]
    ▼
No-op AcoustID consumer (tokio task, logs received IDs, drops them)
← Pass 3 replaces this consumer with the real AcoustID adapter
```

All channels are `tokio::sync::mpsc`. Capacities above are exact — use
them, do not choose different values.

---

### Step 1 — New Crate: `crates/adapters-watcher/`

#### `Cargo.toml`

```toml
[package]
name = "adapters-watcher"
version = "0.1.0"
edition = "2021"

[dependencies]
shared-config   = { path = "../shared-config" }
tokio           = { workspace = true, features = ["sync", "rt"] }
notify          = { version = "6", default-features = false, features = ["macos_fsevent"] }
notify-debouncer-full = "0.3"
tracing         = { workspace = true }
thiserror       = { workspace = true }
```

`notify` must be pinned to version 6. Version 7 changed the
`new_debouncer_opt` API. Do not use a higher version without verifying
the API is compatible.

#### `src/lib.rs`

Define the public surface:

```rust
mod watcher;
mod event;
pub use watcher::MediaWatcher;
pub use event::{FileEvent, FileEventKind};
```

#### `src/event.rs`

```rust
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FileEvent {
    /// Absolute path.
    pub path: PathBuf,
    pub kind: FileEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEventKind {
    /// File was created or modified.
    CreateOrModify,
    /// File was removed.
    Remove,
}
```

#### `src/watcher.rs`

```rust
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::PollWatcher;
use notify_debouncer_full::{new_debouncer_opt, DebounceEventResult, Debouncer};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use shared_config::Config;
use crate::event::{FileEvent, FileEventKind};

pub struct MediaWatcher {
    _debouncer: Debouncer<PollWatcher, notify_debouncer_full::RecommendedCache>,
}

impl MediaWatcher {
    /// Start the watcher. Returns the watcher handle (keep alive) and the
    /// receiver for file events.
    ///
    /// The watcher runs on a std::thread and bridges to Tokio via
    /// blocking_send. The returned mpsc::Receiver must be polled by a
    /// Tokio task.
    pub fn start(
        config: Arc<Config>,
    ) -> Result<(Self, mpsc::Receiver<FileEvent>), WatcherError> {
        let (tx, rx) = mpsc::channel::<FileEvent>(2048);
        let scan_in_progress = Arc::new(AtomicBool::new(false));

        let poll_interval = Duration::from_secs(config.scan_interval_secs);
        let debounce_window = Duration::from_secs(5);
        let watch_path = config.media_root.clone();

        let scan_flag = Arc::clone(&scan_in_progress);
        let tx_clone = tx.clone();

        // Overlap guard + event translation callback
        let handler = move |result: DebounceEventResult| {
            // Acquire overlap guard — skip if scan already in progress
            if scan_flag
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                warn!("watcher: poll tick skipped — previous scan still in progress");
                return;
            }

            let _guard = ScanGuard(Arc::clone(&scan_flag));

            match result {
                Ok(events) => {
                    for event in events {
                        let kind = match event.kind {
                            notify::EventKind::Remove(_) => FileEventKind::Remove,
                            _ => FileEventKind::CreateOrModify,
                        };
                        for path in event.event.paths {
                            if tx_clone
                                .blocking_send(FileEvent { path, kind: kind.clone() })
                                .is_err()
                            {
                                // Receiver dropped — bot is shutting down
                                return;
                            }
                        }
                    }
                }
                Err(errors) => {
                    for e in errors {
                        error!("watcher error: {e}");
                    }
                }
            }
        };

        // PollWatcher ONLY. Never RecommendedWatcher.
        let config_notify = notify::Config::default()
            .with_poll_interval(poll_interval);
        let debouncer = new_debouncer_opt::<_, PollWatcher>(
            debounce_window,
            None,
            handler,
            notify_debouncer_full::RecommendedCache::default(),
            config_notify,
        )
        .map_err(WatcherError::Init)?;

        // Register the watch path recursively
        {
            let mut d = debouncer;  // rebind to get mut
            d.watch(&watch_path, notify::RecursiveMode::Recursive)
                .map_err(WatcherError::Watch)?;
            return Ok((Self { _debouncer: d }, rx));
        }
    }
}

/// RAII guard that clears scan_in_progress on drop.
struct ScanGuard(Arc<AtomicBool>);
impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("watcher init failed: {0}")]
    Init(#[from] notify::Error),
    #[error("watcher watch path failed: {0}")]
    Watch(notify::Error),
}
```

---

### Step 2 — Extend `crates/adapters-media-store/`

This crate now contains three components:
1. **Classifier** — a free function/task factory
2. **Fingerprint Worker** — implements `FingerprintPort` and contains the
   worker task factory
3. **Scanner** — a public struct that wires Classifier + Fingerprint Worker
   together and owns the semaphores

#### Add to `Cargo.toml`

```toml
[dependencies]
# Existing
domain          = { path = "../domain" }
application     = { path = "../application" }
shared-config   = { path = "../shared-config" }
adapters-persistence = { path = "../adapters-persistence" }

# New for this pass
symphonia       = { version = "0.5", features = [
    "mp3", "flac", "ogg", "wav", "aac", "isomp4", "opus",
    "all-formats"
] }
chromaprint-next = { version = "0.4", features = ["parallel"] }
lofty           = "0.21"
tokio           = { workspace = true, features = ["sync", "rt", "fs"] }
tracing         = { workspace = true }
thiserror       = { workspace = true }
uuid            = { workspace = true }
chrono          = { workspace = true }
bytes           = { workspace = true }
```

`chromaprint-next` with `features = ["parallel"]` uses rayon internally for
the fingerprint computation. This is acceptable — the rayon thread pool is
inside chromaprint-next and is not exposed to our code. We still call it via
`tokio::task::spawn_blocking`. Do not add a direct `rayon` dependency.

#### `src/lib.rs`

```rust
mod classifier;
mod fingerprint;
mod tag_reader;
pub mod scanner;

pub use scanner::MediaScanner;
pub use fingerprint::FingerprintAdapter;
```

#### `src/classifier.rs`

The Classifier runs as a single Tokio task. It receives `FileEvent` from the
watcher channel and emits `ToFingerprint` to the fingerprint channel.

```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use adapters_persistence::TrackRepositoryImpl;
use application::ports::TrackRepository;
use adapters_watcher::{FileEvent, FileEventKind};
use shared_config::Config;

/// Message sent from Classifier to Fingerprint Workers.
#[derive(Debug)]
pub struct ToFingerprint {
    pub path:        PathBuf,     // absolute path
    pub mtime:       SystemTime,
    pub size_bytes:  u64,
    pub existing_id: Option<uuid::Uuid>,
}

/// Supported audio extensions. Lowercase only — compare after to_lowercase().
pub static SUPPORTED_EXTENSIONS: &[&str] =
    &["mp3", "flac", "ogg", "wav", "aac", "m4a", "opus"];

pub async fn run_classifier(
    config: Arc<Config>,
    track_repo: Arc<TrackRepositoryImpl>,
    mut file_rx: mpsc::Receiver<FileEvent>,
    fp_tx: mpsc::Sender<ToFingerprint>,
) {
    let supported: HashSet<&str> = SUPPORTED_EXTENSIONS.iter().copied().collect();

    while let Some(event) = file_rx.recv().await {
        match event.kind {
            FileEventKind::Remove => {
                let rel = relative_path(&config.media_root, &event.path);
                if let Err(e) = track_repo.mark_file_missing(&rel).await {
                    warn!("classifier: mark_file_missing failed for {rel}: {e}");
                } else {
                    debug!("classifier: marked file_missing for {rel}");
                }
            }

            FileEventKind::CreateOrModify => {
                // Extension filter — no file read, no semaphore
                let ext = event.path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase());

                match ext {
                    Some(ref e) if supported.contains(e.as_str()) => {}
                    _ => {
                        debug!("classifier: skipping unsupported file {:?}", event.path);
                        continue;
                    }
                }

                // stat() only — no file bytes read, no semaphore needed
                let meta = match std::fs::metadata(&event.path) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("classifier: stat failed for {:?}: {e}", event.path);
                        continue;
                    }
                };

                let mtime = match meta.modified() {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("classifier: mtime unavailable for {:?}: {e}", event.path);
                        continue;
                    }
                };
                let size_bytes = meta.len();
                let rel = relative_path(&config.media_root, &event.path);

                // DB lookup — check if unchanged
                match track_repo.find_by_blob_location(&rel).await {
                    Ok(Some(existing)) => {
                        let db_mtime: Option<SystemTime> = existing
                            .file_modified_at
                            .map(|dt| SystemTime::from(dt));
                        let db_size = existing.file_size_bytes.unwrap_or(-1);

                        if let Some(db_mt) = db_mtime {
                            // Compare with 2-second FAT32/SMB tolerance
                            let unchanged = db_mt
                                .duration_since(mtime)
                                .or_else(|_| mtime.duration_since(db_mt))
                                .map(|d| d.as_secs() < 2)
                                .unwrap_or(false)
                                && db_size == size_bytes as i64;

                            if unchanged {
                                debug!("classifier: skip unchanged {rel}");
                                continue;
                            }
                        }

                        // Changed — send to Fingerprint Worker
                        let _ = fp_tx
                            .send(ToFingerprint {
                                path: event.path,
                                mtime,
                                size_bytes,
                                existing_id: Some(existing.id),
                            })
                            .await;
                    }

                    Ok(None) => {
                        // New file
                        let _ = fp_tx
                            .send(ToFingerprint {
                                path: event.path,
                                mtime,
                                size_bytes,
                                existing_id: None,
                            })
                            .await;
                    }

                    Err(e) => {
                        warn!("classifier: DB lookup failed for {rel}: {e}");
                    }
                }
            }
        }
    }
}

/// Convert absolute path to path relative to media_root.
/// Panics in debug if path is not under media_root.
pub fn relative_path(media_root: &Path, absolute: &Path) -> String {
    absolute
        .strip_prefix(media_root)
        .unwrap_or(absolute)   // fallback: use as-is (log warning in caller)
        .to_string_lossy()
        .into_owned()
}
```

**Important implementation note on mtime comparison:** SMB shares backed by
FAT32 or older NAS firmware have 2-second mtime resolution. Use a 2-second
tolerance window when comparing mtimes, not strict equality. An unchanged file
on a FAT32-backed NAS will consistently show the same 2-second-rounded mtime;
any change will produce a visibly different value.

#### `src/fingerprint.rs`

The Fingerprint Worker pool processes `ToFingerprint` messages. Each message
is handled by a `tokio::task::spawn_blocking` call. Concurrency is limited by
`FINGERPRINT_CONCURRENCY` semaphore.

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{mpsc, Semaphore};
use tracing::{debug, info, warn};
use uuid::Uuid;

use application::ports::{
    enrichment::{AudioFingerprint, RawFileTags},
    FingerprintPort,
    TrackRepository,
};
use application::AppError;
use adapters_persistence::TrackRepositoryImpl;
use domain::{EnrichmentStatus, Track};
use shared_config::Config;

use crate::classifier::ToFingerprint;
use crate::tag_reader::read_file;   // defined below

/// Message emitted to the Enrichment Orchestrator for new tracks.
#[derive(Debug)]
pub struct ToEnrichment {
    pub track_id:     Uuid,
    pub fingerprint:  String,
    pub duration_secs: u32,
}

pub async fn run_fingerprint_worker(
    config: Arc<Config>,
    track_repo: Arc<TrackRepositoryImpl>,
    smb_semaphore: Arc<Semaphore>,
    fp_concurrency: Arc<Semaphore>,
    mut fp_rx: mpsc::Receiver<ToFingerprint>,
    enrich_tx: mpsc::Sender<ToEnrichment>,
) {
    while let Some(msg) = fp_rx.recv().await {
        let repo = Arc::clone(&track_repo);
        let smb = Arc::clone(&smb_semaphore);
        let fp_sem = Arc::clone(&fp_concurrency);
        let tx = enrich_tx.clone();
        let media_root = config.media_root.clone();

        // Each message is handled as a separate spawned task.
        // fp_concurrency limits how many are in-flight simultaneously.
        tokio::spawn(async move {
            let _fp_permit = fp_sem.acquire().await.expect("fp semaphore closed");

            // Acquire SMB permit BEFORE entering spawn_blocking.
            // The permit is moved into the closure and dropped when it returns.
            let smb_permit = smb.acquire_owned().await.expect("smb semaphore closed");

            let path_clone = msg.path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = smb_permit; // holds SMB permit for duration of file reads
                read_file(&path_clone)
            })
            .await;

            match result {
                Err(join_err) => {
                    warn!("fingerprint worker: spawn_blocking panicked: {join_err}");
                    return;
                }
                Ok(Err(io_err)) => {
                    warn!("fingerprint worker: read_file failed for {:?}: {io_err}", msg.path);
                    return;
                }
                Ok(Ok((fingerprint, raw_tags, duration_ms))) => {
                    let rel = crate::classifier::relative_path(&media_root, &msg.path);

                    // DB deduplication by fingerprint
                    match repo.find_by_fingerprint(&fingerprint.fingerprint).await {
                        Ok(Some(existing)) => {
                            if existing.blob_location == rel
                                || msg.existing_id == Some(existing.id)
                            {
                                // Same audio, same location — tags/mtime changed
                                debug!("fingerprint: same audio+location, updating mtime for {rel}");
                                let mtime = chrono::DateTime::from(msg.mtime);
                                let _ = repo.update_file_identity(
                                    existing.id, mtime, msg.size_bytes as i64, &rel
                                ).await;
                            } else {
                                // Same audio, different path — file was moved/renamed
                                info!("fingerprint: file moved {old} → {new}",
                                      old = existing.blob_location, new = rel);
                                let mtime = chrono::DateTime::from(msg.mtime);
                                let _ = repo.update_file_identity(
                                    existing.id, mtime, msg.size_bytes as i64, &rel
                                ).await;
                            }
                        }

                        Ok(None) => {
                            // New audio content — INSERT
                            let mtime = chrono::DateTime::from(msg.mtime);
                            let track = Track {
                                id: Uuid::new_v4(),
                                title: raw_tags.title.unwrap_or_else(|| {
                                    msg.path
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "Unknown".into())
                                }),
                                artist_display: raw_tags.artist,
                                album_id: None,   // resolved during MusicBrainz enrichment
                                track_number: raw_tags.track_number.map(|n| n as i32),
                                disc_number: raw_tags.disc_number.map(|n| n as i32),
                                duration_ms: Some(duration_ms as i32),
                                genre: raw_tags.genre,
                                year: raw_tags.year,
                                audio_fingerprint: Some(fingerprint.fingerprint.clone()),
                                file_modified_at: Some(mtime),
                                file_size_bytes: Some(msg.size_bytes as i64),
                                blob_location: rel.clone(),
                                mbid: None,
                                acoustid_id: None,
                                enrichment_status: EnrichmentStatus::Pending,
                                enrichment_confidence: None,
                                enrichment_attempts: 0,
                                enrichment_locked: false,
                                enriched_at: None,
                                created_at: chrono::Utc::now(),
                                updated_at: chrono::Utc::now(),
                            };

                            match repo.insert(&track).await {
                                Ok(inserted) => {
                                    info!("fingerprint: indexed new track {} [{rel}]", inserted.id);
                                    let _ = tx.send(ToEnrichment {
                                        track_id: inserted.id,
                                        fingerprint: fingerprint.fingerprint,
                                        duration_secs: fingerprint.duration_secs,
                                    }).await;
                                }
                                Err(e) => {
                                    warn!("fingerprint: insert failed for {rel}: {e}");
                                }
                            }
                        }

                        Err(e) => {
                            warn!("fingerprint: DB fingerprint lookup failed: {e}");
                        }
                    }
                }
            }
        });
    }
}
```

#### `src/tag_reader.rs`

This is the single-pass file reader. One SMB read serves Symphonia (PCM
decode for Chromaprint) AND lofty (tag extraction). They both read from
the same file handle — do not open the file twice.

```rust
use std::path::Path;
use std::io::BufReader;
use std::fs::File;

use application::ports::enrichment::{AudioFingerprint, RawFileTags};

pub type TagReaderError = Box<dyn std::error::Error + Send + Sync>;

/// Decode the file at `path`, extract Chromaprint fingerprint and raw tags.
/// Reads first 120 seconds of PCM only. Returns (fingerprint, tags, duration_ms).
///
/// MUST be called from within spawn_blocking. Holds the SMB permit for its
/// entire duration — caller is responsible for acquiring it beforehand.
pub fn read_file(
    path: &Path,
) -> Result<(AudioFingerprint, RawFileTags, u32), TagReaderError> {
    // --- lofty: read tags (sequential, same file) ---
    let tagged_file = lofty::read_from_path(path)?;
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag());

    let raw_tags = RawFileTags {
        title:        tag.and_then(|t| t.title().map(std::string::ToString::to_string)),
        artist:       tag.and_then(|t| t.artist().map(std::string::ToString::to_string)),
        album:        tag.and_then(|t| t.album().map(std::string::ToString::to_string)),
        year:         tag.and_then(|t| t.year()),
        genre:        tag.and_then(|t| t.genre().map(std::string::ToString::to_string)),
        track_number: tag.and_then(|t| t.track()),
        disc_number:  tag.and_then(|t| t.disk()),
        duration_ms:  None, // filled from Symphonia below
    };

    // --- Symphonia: decode PCM for Chromaprint ---
    let file = BufReader::new(File::open(path)?);
    let mss = symphonia::core::io::MediaSourceStream::new(
        Box::new(file),
        Default::default(),
    );

    let probed = symphonia::default::get_probe()
        .format(
            &Default::default(),
            mss,
            &Default::default(),
            &Default::default(),
        )?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("no audio track found")?;

    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("no sample rate")?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(2);

    let duration_secs = track
        .codec_params
        .n_frames
        .zip(track.codec_params.time_base)
        .map(|(frames, tb)| {
            let secs = frames as f64 * tb.numer as f64 / tb.denom as f64;
            secs as u32
        })
        .unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &Default::default())?;

    let mut ctx = chromaprint_next::Context::new(sample_rate as i32, channels as i32);
    ctx.start()?;

    const MAX_DECODE_SECS: u64 = 120;
    let mut decoded_secs: f64 = 0.0;

    loop {
        if decoded_secs >= MAX_DECODE_SECS as f64 {
            break;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(symphonia::core::errors::Error::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let frames = decoded.frames();
        decoded_secs += frames as f64 / sample_rate as f64;

        // Convert to i16 samples for Chromaprint
        let mut samples: Vec<i16> = vec![0i16; frames * channels];
        let spec = *decoded.spec();
        let mut sample_buf =
            symphonia::core::audio::SampleBuffer::<i16>::new(frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);
        samples.copy_from_slice(sample_buf.samples());

        ctx.feed(&samples)?;
    }

    ctx.finish()?;
    let fingerprint_str = ctx.fingerprint()?;

    let duration_ms = if duration_secs > 0 {
        duration_secs * 1000
    } else {
        (decoded_secs * 1000.0) as u32
    };

    Ok((
        AudioFingerprint {
            fingerprint: fingerprint_str,
            duration_secs: duration_secs.max((decoded_secs as u32).max(1)),
        },
        RawFileTags {
            duration_ms: Some(duration_ms),
            ..raw_tags
        },
        duration_ms,
    ))
}
```

#### `src/scanner.rs`

`MediaScanner` is the public facade that wires the watcher, classifier,
and fingerprint worker together. It owns the semaphores and spawns tasks.

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use shared_config::Config;
use adapters_persistence::TrackRepositoryImpl;
use adapters_watcher::MediaWatcher;

use crate::classifier::{run_classifier, ToFingerprint};
use crate::fingerprint::{run_fingerprint_worker, ToEnrichment};

pub struct MediaScanner;

impl MediaScanner {
    /// Start the full scan pipeline.
    ///
    /// Returns:
    /// - A receiver for `ToEnrichment` messages (consumed by the
    ///   Enrichment Orchestrator in apps/bot).
    /// - The `Arc<Semaphore>` SMB_READ_SEMAPHORE (shared with tag writer
    ///   in Pass 4).
    pub fn start(
        config: Arc<Config>,
        track_repo: Arc<TrackRepositoryImpl>,
        token: CancellationToken,
    ) -> (mpsc::Receiver<ToEnrichment>, Arc<Semaphore>) {
        let smb_semaphore = Arc::new(Semaphore::new(config.smb_read_concurrency));
        let fp_concurrency = Arc::new(Semaphore::new(config.fingerprint_concurrency));

        // Start watcher → file_rx
        let (_, file_rx) = MediaWatcher::start(Arc::clone(&config))
            .expect("failed to start MediaWatcher");

        // Classifier channel
        let (fp_tx, fp_rx) = mpsc::channel::<ToFingerprint>(256);
        // Enrichment channel
        let (enrich_tx, enrich_rx) = mpsc::channel::<ToEnrichment>(128);

        // Classifier task
        {
            let config = Arc::clone(&config);
            let repo = Arc::clone(&track_repo);
            let tok = token.clone();
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = tok.cancelled() => {}
                    _ = run_classifier(config, repo, file_rx, fp_tx) => {}
                }
            });
        }

        // Fingerprint Worker task
        {
            let config = Arc::clone(&config);
            let repo = Arc::clone(&track_repo);
            let smb = Arc::clone(&smb_semaphore);
            let fpc = Arc::clone(&fp_concurrency);
            let tok = token.clone();
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = tok.cancelled() => {}
                    _ = run_fingerprint_worker(config, repo, smb, fpc, fp_rx, enrich_tx) => {}
                }
            });
        }

        (enrich_rx, smb_semaphore)
    }
}
```

---

### Step 3 — Enrichment Orchestrator in `crates/application/`

Add `EnrichmentOrchestrator` as a use case. It has two trigger paths:

1. **Reactive:** receives `ToEnrichment { track_id }` from the Fingerprint
   Worker channel and immediately emits it onwards to AcoustID.
2. **Proactive:** polls the DB every `SCAN_INTERVAL_SECS` for tracks that
   are stuck in `pending`/`failed`/`low_confidence`/`unmatched` and should
   be retried (e.g. enrichment failed before, bot was restarted mid-pipeline).

Both paths emit `AcoustIdRequest` to the same output channel.

```rust
// application/src/enrichment_orchestrator.rs

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::ports::TrackRepository;
use crate::AppError;

#[derive(Debug)]
pub struct AcoustIdRequest {
    pub track_id:      Uuid,
    pub fingerprint:   String,
    pub duration_secs: u32,
}

// Import from adapters-media-store at call site in apps/bot.
// The orchestrator receives this type via channel.
#[derive(Debug)]
pub struct ScanResult {
    pub track_id:      Uuid,
    pub fingerprint:   String,
    pub duration_secs: u32,
}

pub struct EnrichmentOrchestrator<R: TrackRepository> {
    pub repo:               Arc<R>,
    pub scan_interval_secs: u64,
}

impl<R: TrackRepository + 'static> EnrichmentOrchestrator<R> {
    pub async fn run(
        &self,
        mut scan_rx: mpsc::Receiver<ScanResult>,
        acoustid_tx: mpsc::Sender<AcoustIdRequest>,
    ) {
        let mut interval = tokio::time::interval(
            Duration::from_secs(self.scan_interval_secs)
        );

        loop {
            tokio::select! {
                biased;

                // Reactive path: new track from Fingerprint Worker
                Some(scan_result) = scan_rx.recv() => {
                    let _ = acoustid_tx.send(AcoustIdRequest {
                        track_id:      scan_result.track_id,
                        fingerprint:   scan_result.fingerprint,
                        duration_secs: scan_result.duration_secs,
                    }).await;
                }

                // Proactive path: DB poll for stale/retryable tracks
                _ = interval.tick() => {
                    match self.repo.claim_for_enrichment(50).await {
                        Ok(tracks) => {
                            for track in tracks {
                                if let (Some(fp), Some(dur)) = (
                                    &track.audio_fingerprint,
                                    track.duration_ms,
                                ) {
                                    let _ = acoustid_tx.send(AcoustIdRequest {
                                        track_id:      track.id,
                                        fingerprint:   fp.clone(),
                                        duration_secs: (dur / 1000) as u32,
                                    }).await;
                                } else {
                                    warn!("orchestrator: track {} has no fingerprint                                            or duration, cannot enrich", track.id);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("orchestrator: claim_for_enrichment failed: {e}");
                        }
                    }
                }
            }
        }
    }
}
```

---

### Step 4 — Complete Persistence Methods in `adapters-persistence/`

Implement all methods marked `todo!()` that are needed by this pass:

#### `update_file_identity`

```sql
UPDATE tracks
SET file_modified_at = $2,
    file_size_bytes  = $3,
    blob_location    = $4,
    updated_at       = now()
WHERE id = $1
```

#### `update_enrichment_status`

```sql
UPDATE tracks
SET enrichment_status  = $2,
    enrichment_attempts = $3,
    enriched_at         = $4,
    updated_at          = now()
WHERE id = $1
```

#### `claim_for_enrichment` — this query must match the master document exactly

```sql
SELECT id, title, artist_display, album_id, track_number, disc_number,
       duration_ms, genre, year, audio_fingerprint, file_modified_at,
       file_size_bytes, blob_location, mbid, acoustid_id,
       enrichment_status, enrichment_confidence, enrichment_attempts,
       enrichment_locked, enriched_at, created_at, updated_at
FROM tracks
WHERE enrichment_locked = false
  AND (
    (enrichment_status IN ('pending', 'failed', 'low_confidence')
     AND enrichment_attempts < $1
     AND (enriched_at IS NULL OR enriched_at < now() - INTERVAL '1 hour'))
    OR
    (enrichment_status = 'unmatched'
     AND enrichment_attempts < $2
     AND enriched_at < now() - INTERVAL '24 hours')
  )
ORDER BY created_at ASC
LIMIT $3
FOR UPDATE SKIP LOCKED
```

Bind `$1 = config.failed_retry_limit`, `$2 = config.unmatched_retry_limit`,
`$3 = limit` (i64). Immediately after fetching, run:

```sql
UPDATE tracks
SET enrichment_status = 'enriching', updated_at = now()
WHERE id = ANY($1)
```

where `$1` is the array of claimed UUIDs. Both queries must run in the same
transaction.

---

### Step 5 — Wire Pipeline in `apps/bot/main.rs`

Add the following to the startup sequence after migrations and the stale
enriching watchdog:

```rust
// 1. Start scan pipeline — returns enrichment receiver and SMB semaphore
let (scan_result_rx, smb_semaphore) = MediaScanner::start(
    Arc::clone(&config),
    Arc::clone(&track_repo),
    token.clone(),
);

// 2. Convert ToEnrichment → ScanResult (same fields, type bridge)
let (orchestrator_scan_tx, orchestrator_scan_rx) = mpsc::channel::<ScanResult>(128);
tokio::spawn(async move {
    let mut rx = scan_result_rx;
    while let Some(msg) = rx.recv().await {
        let _ = orchestrator_scan_tx.send(ScanResult {
            track_id:      msg.track_id,
            fingerprint:   msg.fingerprint,
            duration_secs: msg.duration_secs,
        }).await;
    }
});

// 3. AcoustID channel — no-op consumer in this pass
let (acoustid_tx, mut acoustid_rx) = mpsc::channel::<AcoustIdRequest>(64);
tokio::spawn(async move {
    while let Some(req) = acoustid_rx.recv().await {
        tracing::info!(
            "pass2 no-op: would enrich track {} (fingerprint len={})",
            req.track_id,
            req.fingerprint.len()
        );
    }
});

// 4. Start Enrichment Orchestrator
let orchestrator = Arc::new(EnrichmentOrchestrator {
    repo: Arc::clone(&track_repo),
    scan_interval_secs: config.scan_interval_secs,
});
{
    let tok = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = orchestrator.run(orchestrator_scan_rx, acoustid_tx) => {}
        }
    });
}
```

Store `smb_semaphore` in application state (`Arc<Semaphore>`) — it will be
shared with the Tag Writer in Pass 4. Do not drop it.

---

### Step 6 — Integration Test

Add a test to `crates/adapters-media-store/tests/scan_pipeline.rs` that
verifies the core pipeline contract without a real SMB share:

```rust
// Test: new file → track inserted with enrichment_status = 'pending'
// Test: same file re-scanned with unchanged mtime/size → no DB update
// Test: same file re-scanned with changed mtime → update_file_identity called
// Test: file with duplicate fingerprint → no second insert, only blob_location updated
```

Use a temp directory (`tempfile::TempDir`) and a real Postgres instance
(skip with `#[ignore]` if `TEST_DATABASE_URL` is not set). Do not mock the
DB for these tests — the SQL must be exercised.

---

### Invariants Reminder (Pass 2 Specific)

All 15 master document invariants apply. The following are most likely to be
violated accidentally in this pass:

- **Invariant 1:** `PollWatcher` only. If `notify::RecommendedWatcher` appears
  anywhere, it is wrong.
- **Invariant 3:** `claim_for_enrichment` must filter to `enrichment_status = 'done'`
  for all user-facing queries. The orchestrator claim query is internal only.
- **Invariant 4:** `SMB_READ_SEMAPHORE` permit must be acquired before the
  `spawn_blocking` closure opens any file handle. The permit is moved into the
  closure and dropped when the closure returns.
- **Invariant 6:** No BLAKE3. No `blake3` crate in any `Cargo.toml`.
- **Invariant 9:** `blob_location` stored in DB is always relative to
  `MEDIA_ROOT`. The `relative_path()` helper in `classifier.rs` is the
  single point of this conversion.
- **Invariant 11:** `scan_in_progress` AtomicBool must use `AcqRel`/`Acquire`
  ordering. `Relaxed` is incorrect and will cause missed overlap detection on
  multi-core systems.
- **Invariant 15:** No direct `rayon` dependency. `spawn_blocking` only.

---

### REFERENCE

*[Attach the full contents of `teamti_v2_master.md` here before sending.]*
