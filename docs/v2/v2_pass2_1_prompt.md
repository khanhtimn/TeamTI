# TeamTI v2 — Pass 2.1 Implementation Prompt (Revised)
## Scan Pipeline: Watcher → Classifier → Fingerprint Worker → Enrichment Orchestrator

> This is the authoritative revision. It supersedes the Pass 2 prompt entirely.
> Every ambiguity from Pass 2 is resolved here. Do not reference Pass 2.

---

### Context

Pass 1 is complete. The following are in place and must not be changed unless
a compile error requires it:

- `crates/domain/` — all v2 entities and `EnrichmentStatus`
- `crates/application/` — all port traits
- `crates/shared-config/` — full `Config` with all v2 fields
- `crates/adapters-persistence/` — migrations 0001–0004; partial
  `TrackRepository` impl; remaining methods are `todo!()`

Pass 2.1 implements the complete scan-side pipeline. No external HTTP calls
(AcoustID, MusicBrainz, Cover Art). Those are Pass 3.

---

### Acceptance Criteria

- [ ] `cargo build --workspace` — zero errors, zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] Dropping a supported audio file into `MEDIA_ROOT` produces a `tracks` row
      with `enrichment_status = 'pending'` within `SCAN_INTERVAL_SECS + 10s`
- [ ] Re-scanning an unchanged file (same mtime, same size) causes zero DB writes
- [ ] Re-scanning a moved file updates `blob_location` only; UUID and
      `enrichment_status` are unchanged
- [ ] Deleting an indexed file sets `enrichment_status = 'file_missing'`
- [ ] A duplicate audio file (same Chromaprint, different path) logs a warning
      and inserts no second row
- [ ] `claim_for_enrichment` runs both its SELECT and UPDATE in the same
      transaction and uses `FOR UPDATE SKIP LOCKED`
- [ ] The Enrichment Orchestrator emits to the AcoustID channel (a no-op
      logger in this pass); `enrichment_status` is set to `'enriching'` on claim
- [ ] No two scan-event forwarding batches are in flight simultaneously
      (overlap guard test — see Step 7)

---

### Scope

| Crate | Action |
|---|---|
| `crates/adapters-watcher/` | **Create** |
| `crates/adapters-media-store/` | **Extend** — Classifier, Fingerprint Worker, FingerprintPort impl |
| `crates/adapters-persistence/` | **Complete** — implement `todo!()` methods needed here |
| `crates/application/` | **Extend** — add shared event types + EnrichmentOrchestrator |
| `apps/bot/` | **Extend** — wire pipeline; add no-op AcoustID consumer |

**Does NOT touch:** `adapters-discord`, `adapters-voice`, any HTTP adapter crate.

---

### Channel Topology

```
PollWatcher (std::thread, overlap-guarded)
    │  FileEvent { path, kind }              mpsc capacity: 2048
    ▼
Classifier (single tokio task)
    │  ToFingerprint { path, mtime, size, existing_id }   capacity: 256
    ▼
Fingerprint Worker pool (spawn_blocking, ≤FINGERPRINT_CONCURRENCY)
    │  TrackScanned { track_id, fingerprint, duration_secs }   capacity: 128
    ▼
Enrichment Orchestrator (single tokio task + DB poll every SCAN_INTERVAL_SECS)
    │  AcoustIdRequest { track_id, fingerprint, duration_secs }  capacity: 64
    ▼
No-op AcoustID consumer (this pass: logs and drops)
← Pass 3 replaces this with the real AcoustID adapter
```

All channels are `tokio::sync::mpsc`. Use these exact capacities.

---

### Ambiguity Resolution: Overlap Guard

This section defines exactly what `scan_in_progress: AtomicBool` guards and
why. Read this before implementing the watcher.

**What the AtomicBool guards:** With `PollWatcher`, notify's internal thread
owns the recursive directory walk — we cannot intercept when it starts. What
we can control is whether the resulting events are forwarded downstream. The
AtomicBool therefore guards **event forwarding into the FileEvent channel**,
not the filesystem walk itself.

**Why this is sufficient:** The downstream concern is: do not let a flood of
events from poll cycle N+1 pile into the Classifier channel while poll cycle N
is still being processed. If the AtomicBool is set when cycle N+1 fires, cycle
N+1 events are dropped entirely. The next cycle after N+1 will attempt again.
Because the scan interval is 300s and event processing is much faster, cycles
are rarely skipped in practice.

**Correct implementation contract:**
- Set `scan_in_progress = true` (AcqRel) at the start of the debounce callback
- Use a RAII ScanGuard that stores `Arc<AtomicBool>` and calls
  `store(false, Release)` on drop
- If `compare_exchange(false, true)` fails, log a warning and return immediately
  without forwarding any events
- The ScanGuard must be bound to a named variable in the callback scope —
  not a temporary — to guarantee drop order

**Incorrect patterns (do not use):**
- `store(true, Relaxed)` / `store(false, Relaxed)` — wrong ordering, unsafe on
  multi-core
- Dropping ScanGuard before forwarding all events is complete — wrong ordering
- Spawning a tokio task to forward events and immediately returning — the guard
  drops before forwarding finishes

---

### Ambiguity Resolution: Type Ownership

Pass 2 introduced a redundant type bridge (`ToEnrichment` → `ScanResult`) in
`apps/bot`. This is eliminated. The rule is:

- `TrackScanned` is defined once in `crates/application/src/events.rs`
- `adapters-media-store` imports and emits `TrackScanned`
- `application/EnrichmentOrchestrator` receives `TrackScanned` directly
- `apps/bot` connects them with zero conversion

Similarly, `AcoustIdRequest` is defined in `application/src/events.rs`.
Pass 3's AcoustID adapter imports it from there.

---

### Step 1 — `crates/application/src/events.rs` (new file)

Add this file. These are pipeline event types, not port traits.

```rust
use uuid::Uuid;

/// Emitted by the Fingerprint Worker when a new or changed track is indexed.
/// Received by the Enrichment Orchestrator.
#[derive(Debug, Clone)]
pub struct TrackScanned {
    pub track_id:      Uuid,
    pub fingerprint:   String,
    pub duration_secs: u32,
}

/// Emitted by the Enrichment Orchestrator to the AcoustID adapter.
#[derive(Debug, Clone)]
pub struct AcoustIdRequest {
    pub track_id:      Uuid,
    pub fingerprint:   String,
    pub duration_secs: u32,
}
```

Add `pub mod events; pub use events::{TrackScanned, AcoustIdRequest};`
to `application/src/lib.rs`.

---

### Step 2 — `crates/application/src/enrichment_orchestrator.rs` (new file)

`EnrichmentOrchestrator` owns no concrete types — only `Arc<dyn TrackRepository>`.

```rust
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::AppError;
use crate::events::{AcoustIdRequest, TrackScanned};
use crate::ports::TrackRepository;

pub struct EnrichmentOrchestrator {
    pub repo:               Arc<dyn TrackRepository>,
    pub scan_interval_secs: u64,
    pub failed_retry_limit: u32,
    pub unmatched_retry_limit: u32,
}

impl EnrichmentOrchestrator {
    pub async fn run(
        self: Arc<Self>,
        mut scan_rx: mpsc::Receiver<TrackScanned>,
        acoustid_tx: mpsc::Sender<AcoustIdRequest>,
    ) {
        let mut interval =
            tokio::time::interval(Duration::from_secs(self.scan_interval_secs));
        // The first tick fires immediately — skip it so we don't claim
        // tracks before the DB connection is fully warmed.
        interval.tick().await;

        loop {
            tokio::select! {
                biased;

                // Reactive: new track from Fingerprint Worker
                Some(scanned) = scan_rx.recv() => {
                    info!("orchestrator: reactive enrich for track {}", scanned.track_id);
                    let _ = acoustid_tx.send(AcoustIdRequest {
                        track_id:      scanned.track_id,
                        fingerprint:   scanned.fingerprint,
                        duration_secs: scanned.duration_secs,
                    }).await;
                }

                // Proactive: DB poll for retryable tracks
                _ = interval.tick() => {
                    let claimed = self.repo
                        .claim_for_enrichment(
                            self.failed_retry_limit,
                            self.unmatched_retry_limit,
                            50,
                        )
                        .await;

                    match claimed {
                        Ok(tracks) => {
                            info!("orchestrator: claimed {} tracks for enrichment",
                                  tracks.len());
                            for track in tracks {
                                match (&track.audio_fingerprint, track.duration_ms) {
                                    (Some(fp), Some(dur)) => {
                                        let _ = acoustid_tx.send(AcoustIdRequest {
                                            track_id:      track.id,
                                            fingerprint:   fp.clone(),
                                            duration_secs: (dur / 1000) as u32,
                                        }).await;
                                    }
                                    _ => {
                                        warn!(
                                            "orchestrator: track {} missing fingerprint \
                                             or duration, cannot enrich — skipping",
                                            track.id
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("orchestrator: claim_for_enrichment error: {e}"),
                    }
                }
            }
        }
    }
}
```

Update `application/src/lib.rs`:
```rust
pub mod enrichment_orchestrator;
pub use enrichment_orchestrator::EnrichmentOrchestrator;
```

---

### Step 3 — `crates/adapters-watcher/`

#### `Cargo.toml`

```toml
[package]
name = "adapters-watcher"
version = "0.1.0"
edition = "2021"

[dependencies]
shared-config        = { path = "../shared-config" }
tokio                = { workspace = true, features = ["sync"] }
notify               = { version = "6", default-features = false }
notify-debouncer-full = { version = "0.3", default-features = false }
tracing              = { workspace = true }
thiserror            = { workspace = true }
```

`default-features = false` on notify prevents pulling in platform-specific
watcher backends (inotify, kqueue, FSEvents). We only need the polling
backend. No platform feature flags.

#### `src/lib.rs`

```rust
mod event;
mod watcher;

pub use event::{FileEvent, FileEventKind};
pub use watcher::{MediaWatcher, WatcherError};
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
    CreateOrModify,
    Remove,
}
```

#### `src/watcher.rs`

Read the "Ambiguity Resolution: Overlap Guard" section above before
implementing this. The ScanGuard pattern is mandatory.

```rust
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::PollWatcher;
use notify_debouncer_full::{new_debouncer_opt, DebounceEventResult};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use shared_config::Config;
use crate::event::{FileEvent, FileEventKind};

pub struct MediaWatcher {
    // Held alive for the lifetime of the watcher.
    // Dropping this stops the poll loop.
    _debouncer: Box<dyn std::any::Any + Send>,
}

/// RAII guard: stores flag, calls store(false, Release) on drop.
/// Must be bound to a named `let` binding in every callback invocation
/// to guarantee it is not dropped until all event forwarding is complete.
struct ScanGuard(Arc<AtomicBool>);

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl MediaWatcher {
    pub fn start(
        config: Arc<Config>,
    ) -> Result<(Self, mpsc::Receiver<FileEvent>), WatcherError> {
        let (tx, rx) = mpsc::channel::<FileEvent>(2048);
        let scan_in_progress = Arc::new(AtomicBool::new(false));
        let watch_path = config.media_root.clone();
        let poll_interval = Duration::from_secs(config.scan_interval_secs);
        // 5-second debounce absorbs chunked writes and rapid successive events.
        let debounce_window = Duration::from_secs(5);

        let flag = Arc::clone(&scan_in_progress);
        let tx_cb = tx.clone();

        let callback = move |result: DebounceEventResult| {
            // --- Overlap guard: try to set flag from false → true ---
            // If already true, a previous batch is still forwarding. Skip.
            if flag
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                warn!("watcher: poll cycle skipped — previous batch still in progress");
                return;
            }
            // SAFETY: guard must be a named binding. If it were a temporary,
            // it would drop immediately, clearing the flag before forwarding
            // is complete. Clippy may warn about unused variable — suppress it.
            #[allow(unused_variables)]
            let _guard = ScanGuard(Arc::clone(&flag));

            match result {
                Err(errors) => {
                    for e in errors {
                        error!("watcher: notify error: {e}");
                    }
                    // _guard drops here, clearing the flag
                }
                Ok(events) => {
                    for debounced in events {
                        let kind = match debounced.kind {
                            notify::EventKind::Remove(_) => FileEventKind::Remove,
                            _ => FileEventKind::CreateOrModify,
                        };
                        for path in debounced.event.paths {
                            // blocking_send: this callback runs on a std::thread,
                            // not inside the Tokio runtime.
                            if tx_cb.blocking_send(FileEvent {
                                path,
                                kind: kind.clone(),
                            }).is_err() {
                                // Receiver dropped — bot is shutting down.
                                // Return immediately; _guard drops and clears flag.
                                return;
                            }
                        }
                    }
                    // _guard drops here after all events are forwarded
                }
            }
        };

        let notify_config = notify::Config::default()
            .with_poll_interval(poll_interval);

        // PollWatcher ONLY. The type parameter is explicit and mandatory.
        // Never substitute RecommendedWatcher here.
        let mut debouncer = new_debouncer_opt::<_, PollWatcher>(
            debounce_window,
            None,
            callback,
            notify_debouncer_full::RecommendedCache::default(),
            notify_config,
        )
        .map_err(WatcherError::Init)?;

        debouncer
            .watch(&watch_path, notify::RecursiveMode::Recursive)
            .map_err(WatcherError::Watch)?;

        // Box<dyn Any> erases the concrete Debouncer type.
        // The debouncer is kept alive until MediaWatcher is dropped.
        Ok((
            Self {
                _debouncer: Box::new(debouncer),
            },
            rx,
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("watcher init error: {0}")]
    Init(#[from] notify::Error),
    #[error("watcher watch path error: {0}")]
    Watch(notify::Error),
}
```

---

### Step 4 — `crates/adapters-media-store/`

#### `Cargo.toml` additions

```toml
[dependencies]
domain               = { path = "../domain" }
application          = { path = "../application" }
shared-config        = { path = "../shared-config" }
adapters-persistence = { path = "../adapters-persistence" }
adapters-watcher     = { path = "../adapters-watcher" }

symphonia = { version = "0.5", default-features = false, features = [
    "mp3", "flac", "ogg", "wav", "aac", "isomp4", "opus",
    "symphonia-format-ogg",
    "symphonia-codec-vorbis",
    "symphonia-codec-flac",
    "symphonia-codec-mp3",
    "symphonia-codec-aac",
    "symphonia-codec-opus",
] }
chromaprint-next  = { version = "0.4", features = ["parallel"] }
lofty             = "0.21"
tokio             = { workspace = true, features = ["sync", "rt", "fs"] }
tracing           = { workspace = true }
thiserror         = { workspace = true }
uuid              = { workspace = true }
chrono            = { workspace = true }
bytes             = { workspace = true }
```

`symphonia default-features = false` avoids pulling in all codecs.
List only the formats and codecs matching the supported extension list
from §7 of the master document: mp3, flac, ogg/vorbis, wav, aac, m4a/isomp4,
opus. Do not enable `all-formats` — it increases binary size significantly
for no benefit.

#### Module structure

```
adapters-media-store/src/
  lib.rs
  classifier.rs       ← Classifier task
  fingerprint.rs      ← Fingerprint Worker pool
  tag_reader.rs       ← single-pass file reader (spawn_blocking only)
  scanner.rs          ← MediaScanner facade
```

#### `src/lib.rs`

```rust
mod classifier;
mod fingerprint;
mod tag_reader;
pub mod scanner;

pub use scanner::MediaScanner;
```

#### `src/classifier.rs`

```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tracing::{debug, warn};

use adapters_persistence::TrackRepositoryImpl;
use application::ports::repository::TrackRepository;
use adapters_watcher::{FileEvent, FileEventKind};
use shared_config::Config;

/// Extensions compared after `.to_ascii_lowercase()`.
pub const SUPPORTED_EXTENSIONS: &[&str] =
    &["mp3", "flac", "ogg", "wav", "aac", "m4a", "opus"];

/// 2-second tolerance for FAT32/SMB mtime resolution.
/// Files on FAT32-backed NAS shares have 2-second mtime granularity.
/// A difference below this threshold is treated as equal.
const MTIME_TOLERANCE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct ToFingerprint {
    pub path:        PathBuf,   // absolute
    pub mtime:       SystemTime,
    pub size_bytes:  u64,
    pub existing_id: Option<uuid::Uuid>,
}

pub async fn run_classifier(
    config: Arc<Config>,
    track_repo: Arc<TrackRepositoryImpl>,
    mut file_rx: mpsc::Receiver<FileEvent>,
    fp_tx: mpsc::Sender<ToFingerprint>,
) {
    let supported: HashSet<&'static str> =
        SUPPORTED_EXTENSIONS.iter().copied().collect();

    while let Some(event) = file_rx.recv().await {
        match event.kind {
            FileEventKind::Remove => {
                let rel = to_relative(&config.media_root, &event.path);
                if let Err(e) = track_repo.mark_file_missing(&rel).await {
                    warn!("classifier: mark_file_missing({rel}): {e}");
                }
            }

            FileEventKind::CreateOrModify => {
                // Extension check — no I/O
                let ext = event.path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase());

                let supported_ext = ext
                    .as_deref()
                    .map(|e| supported.contains(e))
                    .unwrap_or(false);

                if !supported_ext {
                    debug!("classifier: skip unsupported {:?}", event.path);
                    continue;
                }

                // stat() only — no file bytes, no semaphore
                let meta = match std::fs::metadata(&event.path) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("classifier: stat({:?}): {e}", event.path);
                        continue;
                    }
                };
                let Ok(mtime) = meta.modified() else {
                    warn!("classifier: mtime unavailable for {:?}", event.path);
                    continue;
                };
                let size_bytes = meta.len();
                let rel = to_relative(&config.media_root, &event.path);

                // DB lookup for change detection
                let existing = match track_repo.find_by_blob_location(&rel).await {
                    Ok(row) => row,
                    Err(e) => {
                        warn!("classifier: DB lookup({rel}): {e}");
                        continue;
                    }
                };

                if let Some(ref track) = existing {
                    if let Some(db_mtime) = track.file_modified_at {
                        let db_st = SystemTime::from(db_mtime);
                        let same_mtime = mtime_within_tolerance(mtime, db_st, MTIME_TOLERANCE);
                        let same_size = track.file_size_bytes == Some(size_bytes as i64);

                        if same_mtime && same_size {
                            debug!("classifier: unchanged {rel}");
                            continue; // skip — no write needed
                        }
                    }
                }

                let _ = fp_tx
                    .send(ToFingerprint {
                        path: event.path,
                        mtime,
                        size_bytes,
                        existing_id: existing.map(|t| t.id),
                    })
                    .await;
            }
        }
    }
}

/// Convert absolute path to path relative to media_root.
/// Uses strip_prefix; falls back to the full path string with a warning.
pub fn to_relative(media_root: &Path, absolute: &Path) -> String {
    match absolute.strip_prefix(media_root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => {
            warn!("classifier: path {:?} is not under media_root {:?}",
                  absolute, media_root);
            absolute.to_string_lossy().into_owned()
        }
    }
}

/// Returns true if the absolute difference between two SystemTime values
/// is less than `tolerance`. Uses saturating arithmetic to avoid panics.
fn mtime_within_tolerance(a: SystemTime, b: SystemTime, tolerance: Duration) -> bool {
    let diff = if a >= b {
        a.duration_since(b)
    } else {
        b.duration_since(a)
    };
    diff.map(|d| d < tolerance).unwrap_or(false)
}
```

#### `src/tag_reader.rs`

This function is called exclusively from inside `tokio::task::spawn_blocking`.
It must be `Send` (all types it uses must be `Send`). It holds the SMB permit
for its entire duration — callers acquire the permit before calling
`spawn_blocking` and move the permit into the closure.

**Single-pass contract:** The file is opened once. `lofty` reads tags from
it, then `symphonia` decodes PCM from it. Do NOT open the file twice.

```rust
use std::io::{BufReader, Seek, SeekFrom};
use std::fs::File;
use std::path::Path;

use application::ports::enrichment::{AudioFingerprint, RawFileTags};

pub type TagReaderError = Box<dyn std::error::Error + Send + Sync>;

/// Decode `path`, returning (fingerprint, raw_tags, duration_ms).
///
/// Contract:
/// - Called from spawn_blocking only.
/// - Caller holds SMB_READ_SEMAPHORE for the duration.
/// - Reads the first 120 seconds of PCM for Chromaprint.
///
/// Error handling: all errors are propagated as Box<dyn Error>. The caller
/// logs the error with the path context and discards the message.
pub fn read_file(path: &Path) -> Result<(AudioFingerprint, RawFileTags, u32), TagReaderError> {
    // --- Pass 1: lofty tag extraction ---
    // lofty reads the entire tag section (usually at file start or end).
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
        duration_ms:  None, // filled below from Symphonia
    };

    // --- Pass 2: Symphonia PCM decode → Chromaprint ---
    // Open a new file handle (lofty does not expose its file position).
    // This is two file open() calls but ONE logical read — both are
    // sequential reads of the same file and both are within the single
    // spawn_blocking call holding the SMB permit.
    let file = BufReader::new(File::open(path)?);
    let mss = symphonia::core::io::MediaSourceStream::new(
        Box::new(file),
        Default::default(),
    );

    let hint = {
        let mut h = symphonia::core::probe::Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            h.with_extension(ext);
        }
        h
    };

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &Default::default(),
        &Default::default(),
    )?;

    let mut format = probed.format;

    // Select the first decodable audio track.
    let track = format
        .tracks()
        .iter()
        .find(|t| {
            t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
        })
        .ok_or("no decodable audio track found")?;

    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("track missing sample_rate")?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(2)
        .min(2); // Chromaprint expects mono or stereo; clamp to 2

    // Estimate total duration from codec params (may be 0 for VBR/streaming).
    let duration_secs_estimate = track
        .codec_params
        .n_frames
        .zip(track.codec_params.time_base)
        .map(|(frames, tb)| {
            (frames as f64 * tb.numer as f64 / tb.denom as f64) as u32
        })
        .unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &Default::default())?;

    let mut ctx = chromaprint_next::Context::new(
        sample_rate as i32,
        channels as i32,
    );
    ctx.start()?;

    const MAX_DECODE_SECS: f64 = 120.0;
    let mut decoded_secs: f64 = 0.0;

    'decode: loop {
        if decoded_secs >= MAX_DECODE_SECS {
            break;
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            // IoError at EOF is normal — stop gracefully.
            Err(symphonia::core::errors::Error::IoError(_)) => break 'decode,
            // ResetRequired: decoder state changed (e.g. gapless).
            Err(symphonia::core::errors::Error::ResetRequired) => {
                decoder.reset();
                continue;
            }
            // Other errors: skip this packet, do not abort.
            Err(_) => continue,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let spec = *decoded.spec();
        let frame_count = decoded.frames();
        decoded_secs += frame_count as f64 / sample_rate as f64;

        // Convert decoded buffer to interleaved i16 samples.
        // SampleBuffer is allocated once per packet (small allocation on stack
        // equivalent; no heap pressure for typical 1152-4096 frame packets).
        let mut sample_buf =
            symphonia::core::audio::SampleBuffer::<i16>::new(frame_count as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);

        ctx.feed(sample_buf.samples())?;
    }

    ctx.finish()?;
    let fingerprint_str = ctx.fingerprint()?;

    // Use the codec-params estimate if available; fall back to decoded time.
    let duration_secs = if duration_secs_estimate > 0 {
        duration_secs_estimate
    } else {
        (decoded_secs as u32).max(1)
    };
    let duration_ms = duration_secs * 1000;

    Ok((
        AudioFingerprint {
            fingerprint: fingerprint_str,
            duration_secs,
        },
        RawFileTags {
            duration_ms: Some(duration_ms),
            ..raw_tags
        },
        duration_ms,
    ))
}
```

#### `src/fingerprint.rs`

**Concurrency model:** The Fingerprint Worker is a single tokio task that
reads from `fp_rx` and spawns a new tokio task per message. Each spawned task:

1. Acquires `fp_concurrency` permit — limits how many are actively doing
   CPU-bound work. Tasks wait for a permit before proceeding; the queue is
   unbounded (limited only by the channel capacity of 256).
2. Acquires `smb_semaphore` permit (owned) — gates NAS reads.
3. Calls `spawn_blocking` — moves the owned SMB permit into the closure.
4. On return, does DB work and emits to enrichment channel.

The `fp_concurrency` permit is held across the entire task (steps 2–4),
not just inside `spawn_blocking`, because we want to limit total concurrent
SMB+CPU operations, not just the blocking portion.

```rust
use std::sync::Arc;
use std::path::PathBuf;

use tokio::sync::{mpsc, Semaphore, OwnedSemaphorePermit};
use tracing::{debug, info, warn};
use uuid::Uuid;
use chrono::DateTime;

use application::events::TrackScanned;
use application::ports::repository::TrackRepository;
use adapters_persistence::TrackRepositoryImpl;
use domain::{EnrichmentStatus, Track};
use shared_config::Config;

use crate::classifier::{ToFingerprint, to_relative};
use crate::tag_reader::read_file;

pub async fn run_fingerprint_worker(
    config: Arc<Config>,
    track_repo: Arc<TrackRepositoryImpl>,
    smb_semaphore: Arc<Semaphore>,
    fp_concurrency: Arc<Semaphore>,
    mut fp_rx: mpsc::Receiver<ToFingerprint>,
    scan_tx: mpsc::Sender<TrackScanned>,
) {
    while let Some(msg) = fp_rx.recv().await {
        let repo    = Arc::clone(&track_repo);
        let smb     = Arc::clone(&smb_semaphore);
        let fp_sem  = Arc::clone(&fp_concurrency);
        let tx      = scan_tx.clone();
        let cfg     = Arc::clone(&config);

        tokio::spawn(async move {
            // Step 1: Acquire fp_concurrency permit.
            // This task waits here until a slot is free.
            let _fp_permit = fp_sem.acquire_owned().await
                .expect("fp_concurrency semaphore closed");

            // Step 2: Acquire SMB permit (owned so it can cross spawn_blocking).
            let smb_permit: OwnedSemaphorePermit = smb.acquire_owned().await
                .expect("smb_semaphore closed");

            // Step 3: spawn_blocking — SMB permit is moved in and drops on return.
            let path = msg.path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = smb_permit; // drops when closure returns
                read_file(&path)
            })
            .await;

            // Step 4: Handle result and write to DB.
            match result {
                Err(join_err) => {
                    warn!("fingerprint: spawn_blocking panic for {:?}: {join_err}",
                          msg.path);
                    return;
                }
                Ok(Err(e)) => {
                    warn!("fingerprint: read_file failed for {:?}: {e}", msg.path);
                    return;
                }
                Ok(Ok((fp, raw_tags, duration_ms))) => {
                    let rel = to_relative(&cfg.media_root, &msg.path);
                    let mtime: DateTime<chrono::Utc> = msg.mtime.into();

                    match repo.find_by_fingerprint(&fp.fingerprint).await {
                        Err(e) => {
                            warn!("fingerprint: DB fingerprint lookup failed: {e}");
                        }

                        Ok(Some(existing)) => {
                            let same_location = existing.blob_location == rel;
                            let same_id = msg.existing_id == Some(existing.id);

                            if same_location || same_id {
                                // Same audio content, same or expected location.
                                // Only mtime/size changed (e.g. tag writeback).
                                debug!("fingerprint: mtime/size update for {rel}");
                                let _ = repo.update_file_identity(
                                    existing.id, mtime, msg.size_bytes as i64, &rel,
                                ).await;
                            } else {
                                // Same audio, different path — file was moved/renamed.
                                info!("fingerprint: moved {} → {}", existing.blob_location, rel);
                                let _ = repo.update_file_identity(
                                    existing.id, mtime, msg.size_bytes as i64, &rel,
                                ).await;
                                // Do NOT re-enrich; identity is preserved.
                            }
                        }

                        Ok(None) => {
                            // New audio content — insert and enqueue for enrichment.
                            let title = raw_tags.title.unwrap_or_else(|| {
                                msg.path
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "Unknown".into())
                            });

                            let track = Track {
                                id:                  Uuid::new_v4(),
                                title,
                                artist_display:       raw_tags.artist,
                                album_id:             None,
                                track_number:         raw_tags.track_number.map(|n| n as i32),
                                disc_number:          raw_tags.disc_number.map(|n| n as i32),
                                duration_ms:          Some(duration_ms as i32),
                                genre:                raw_tags.genre,
                                year:                 raw_tags.year,
                                audio_fingerprint:    Some(fp.fingerprint.clone()),
                                file_modified_at:     Some(mtime),
                                file_size_bytes:      Some(msg.size_bytes as i64),
                                blob_location:        rel,
                                mbid:                 None,
                                acoustid_id:          None,
                                enrichment_status:    EnrichmentStatus::Pending,
                                enrichment_confidence: None,
                                enrichment_attempts:   0,
                                enrichment_locked:     false,
                                enriched_at:           None,
                                created_at:            chrono::Utc::now(),
                                updated_at:            chrono::Utc::now(),
                            };

                            match repo.insert(&track).await {
                                Ok(inserted) => {
                                    info!("fingerprint: indexed {} ({})",
                                          inserted.id, inserted.blob_location);
                                    let _ = tx.send(TrackScanned {
                                        track_id:      inserted.id,
                                        fingerprint:   fp.fingerprint,
                                        duration_secs: fp.duration_secs,
                                    }).await;
                                }
                                Err(e) => {
                                    warn!("fingerprint: insert failed: {e}");
                                }
                            }
                        }
                    }
                }
            }
            // _fp_permit drops here, freeing the fp_concurrency slot.
        });
    }
}
```

#### `src/scanner.rs`

`MediaScanner` is the public facade. It constructs the semaphores and spawns
all tasks. It returns the `scan_rx` receiver and the `smb_semaphore` (the
semaphore must be stored in app state — it is shared with the Tag Writer in
Pass 4).

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use application::events::TrackScanned;
use adapters_persistence::TrackRepositoryImpl;
use adapters_watcher::MediaWatcher;
use shared_config::Config;

use crate::classifier::{ToFingerprint, run_classifier};
use crate::fingerprint::run_fingerprint_worker;

pub struct MediaScanner;

impl MediaScanner {
    /// Start the scan pipeline.
    ///
    /// Returns:
    /// - `mpsc::Receiver<TrackScanned>` — connect to EnrichmentOrchestrator
    /// - `Arc<Semaphore>` — SMB_READ_SEMAPHORE, store in AppState for Pass 4
    pub fn start(
        config: Arc<Config>,
        track_repo: Arc<TrackRepositoryImpl>,
        token: CancellationToken,
    ) -> (mpsc::Receiver<TrackScanned>, Arc<Semaphore>) {
        let smb_semaphore   = Arc::new(Semaphore::new(config.smb_read_concurrency));
        let fp_concurrency  = Arc::new(Semaphore::new(config.fingerprint_concurrency));

        let (_, file_rx) = MediaWatcher::start(Arc::clone(&config))
            .expect("MediaWatcher failed to start");

        let (fp_tx,   fp_rx)   = mpsc::channel::<ToFingerprint>(256);
        let (scan_tx, scan_rx) = mpsc::channel::<TrackScanned>(128);

        // Classifier task
        spawn_with_cancel(token.clone(), {
            let config = Arc::clone(&config);
            let repo   = Arc::clone(&track_repo);
            async move { run_classifier(config, repo, file_rx, fp_tx).await }
        });

        // Fingerprint Worker task
        spawn_with_cancel(token.clone(), {
            let config = Arc::clone(&config);
            let repo   = Arc::clone(&track_repo);
            let smb    = Arc::clone(&smb_semaphore);
            let fpc    = Arc::clone(&fp_concurrency);
            async move {
                run_fingerprint_worker(config, repo, smb, fpc, fp_rx, scan_tx).await
            }
        });

        (scan_rx, smb_semaphore)
    }
}

fn spawn_with_cancel<F>(token: CancellationToken, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = token.cancelled() => {}
            _ = fut => {}
        }
    });
}
```

---

### Step 5 — Complete Persistence Methods

#### Update `TrackRepository` trait in `application/src/ports/repository.rs`

The `claim_for_enrichment` signature must accept the retry limit parameters
explicitly — the trait cannot access `Config` directly.

```rust
async fn claim_for_enrichment(
    &self,
    failed_retry_limit: u32,
    unmatched_retry_limit: u32,
    limit: i64,
) -> Result<Vec<Track>, AppError>;
```

Update all existing trait implementations and the `todo!()` stub to match.

#### `update_file_identity` implementation

```rust
sqlx::query!(
    r#"
    UPDATE tracks
    SET file_modified_at = $2,
        file_size_bytes  = $3,
        blob_location    = $4,
        updated_at       = now()
    WHERE id = $1
    "#,
    id, file_modified_at, file_size_bytes, blob_location
)
.execute(&self.pool)
.await?;
Ok(())
```

#### `update_enrichment_status` implementation

```rust
sqlx::query!(
    r#"
    UPDATE tracks
    SET enrichment_status   = $2,
        enrichment_attempts = $3,
        enriched_at         = $4,
        updated_at          = now()
    WHERE id = $1
    "#,
    id,
    status.to_string(),
    attempts,
    enriched_at
)
.execute(&self.pool)
.await?;
Ok(())
```

#### `claim_for_enrichment` — must run in a single transaction

Both the SELECT and the UPDATE must be in the same `sqlx` transaction.
If the UPDATE fails, the SELECT is rolled back and the rows are not claimed.

```rust
async fn claim_for_enrichment(
    &self,
    failed_retry_limit: u32,
    unmatched_retry_limit: u32,
    limit: i64,
) -> Result<Vec<Track>, AppError> {
    let mut tx = self.pool.begin().await?;

    let rows = sqlx::query_as::<_, Track>(
        r#"
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
        "#,
    )
    .bind(failed_retry_limit as i32)
    .bind(unmatched_retry_limit as i32)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;

    if !rows.is_empty() {
        let ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
        sqlx::query!(
            r#"
            UPDATE tracks
            SET enrichment_status = 'enriching',
                updated_at        = now()
            WHERE id = ANY($1)
            "#,
            &ids as &[uuid::Uuid]
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(rows)
}
```

---

### Step 6 — Wire in `apps/bot/main.rs`

After migrations and the stale enriching watchdog:

```rust
use application::{EnrichmentOrchestrator, events::AcoustIdRequest};
use adapters_media_store::scanner::MediaScanner;
use tokio::sync::mpsc;

// 1. Start scan pipeline
let (scan_rx, smb_semaphore) = MediaScanner::start(
    Arc::clone(&config),
    Arc::clone(&track_repo),
    token.clone(),
);

// 2. AcoustID channel — no-op consumer until Pass 3
let (acoustid_tx, mut acoustid_rx) = mpsc::channel::<AcoustIdRequest>(64);
tokio::spawn(async move {
    while let Some(req) = acoustid_rx.recv().await {
        tracing::info!(
            "pass2 stub: pending enrich for track_id={} fingerprint_len={}",
            req.track_id,
            req.fingerprint.len(),
        );
    }
});

// 3. Enrichment Orchestrator
let orchestrator = Arc::new(EnrichmentOrchestrator {
    repo:                   Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
    scan_interval_secs:     config.scan_interval_secs,
    failed_retry_limit:     config.failed_retry_limit,
    unmatched_retry_limit:  config.unmatched_retry_limit,
});
{
    let tok = token.clone();
    let o   = Arc::clone(&orchestrator);
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = o.run(scan_rx, acoustid_tx) => {}
        }
    });
}

// 4. Store smb_semaphore in TypeMap for use by Tag Writer in Pass 4.
//    Key type defined in adapters-media-store or shared-config.
{
    let mut data = client.data.write().await;
    data.insert::<SmbSemaphoreKey>(Arc::clone(&smb_semaphore));
}
```

Define `SmbSemaphoreKey` as a Serenity `TypeMapKey`:

```rust
// In adapters-media-store/src/scanner.rs or a shared types module
use serenity::prelude::TypeMapKey;
pub struct SmbSemaphoreKey;
impl TypeMapKey for SmbSemaphoreKey {
    type Value = Arc<tokio::sync::Semaphore>;
}
```

---

### Step 7 — Tests

Add `crates/adapters-media-store/tests/scan_pipeline.rs`.

Each test must use a real temp directory and a real Postgres instance.
Guard with:
```rust
fn db_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}
// At top of each test:
let Some(db_url) = db_url() else { return; };
```

**Required tests:**

```
test_new_file_indexed
  → Drop a real mp3/flac into temp dir.
  → Trigger CreateOrModify event manually through classifier.
  → Assert: tracks row exists with enrichment_status = 'pending'.

test_unchanged_file_skipped
  → Index a file (run test_new_file_indexed flow).
  → Send same path again with same mtime and size.
  → Assert: no second INSERT; updated_at unchanged.

test_file_moved
  → Index a file.
  → Send Remove for old path, CreateOrModify for new path.
  → Assert: blob_location updated; UUID unchanged; enrichment_status unchanged.

test_file_deleted
  → Index a file.
  → Send Remove event.
  → Assert: enrichment_status = 'file_missing'.

test_duplicate_fingerprint
  → Index file A.
  → Send CreateOrModify for file B with identical audio content.
  → Assert: only one tracks row; blob_location not changed to B;
    warning log emitted.

test_claim_for_enrichment_transaction
  → Insert 3 pending tracks directly via SQL.
  → Call claim_for_enrichment(5, 3, 50).
  → Assert: returns 3 tracks; all have enrichment_status = 'enriching' in DB.
  → Assert: calling again immediately returns 0 tracks (SKIP LOCKED).
```

---

### Invariants (Pass 2.1 Specific)

All 15 master document invariants apply. Highest-risk for this pass:

| Invariant | Risk |
|---|---|
| 1 — PollWatcher only | Compiler accepts `RecommendedWatcher`; SMB silently produces zero events |
| 4 — SMB semaphore before file bytes | Permit must be `acquire_owned()`, not `acquire()`, to be `Send` into spawn_blocking |
| 6 — No BLAKE3 | Search `Cargo.toml` files for `blake3`; CI should fail if found |
| 9 — blob_location relative | `to_relative()` is the single conversion point; never call `.to_string_lossy()` on an absolute path for DB storage |
| 11 — AtomicBool AcqRel/Acquire | `Relaxed` compiles but is unsound on multi-core; compare_exchange must use `AcqRel`/`Acquire` |
| 15 — No direct rayon | `chromaprint-next` uses rayon internally; that is acceptable; we must not add `rayon` as a direct dependency |

---

### REFERENCE

*[Attach full `teamti_v2_master.md` here before sending to agent.]*
