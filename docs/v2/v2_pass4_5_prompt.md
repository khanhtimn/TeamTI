# TeamTI v2 — Pass 4.5 Implementation Prompt
## Unified Error Handling, Observability & Traceability

> This is a cross-cutting refactoring pass. It touches every crate.
> No new features are added. Every change improves error ergonomics,
> diagnostic clarity, or observability. This pass must be completed
> before Pass 5 (Discord commands), as Pass 5 depends on clean,
> actionable errors surfacing to users.

---

### Objectives

The current codebase has four error problems:

1. **Fragmentation** — each crate defines its own ad-hoc error types.
   `Box<dyn std::error::Error + Send + Sync>` appears as a return type
   in tag_reader and tag_writer. `AppError::ExternalApi(String)` is a
   catch-all that loses all structure.

2. **No retryability signal** — the enrichment state machine decides
   retry policy by comparing attempt counts to config limits, with no
   input from the error itself. A DNS failure and a 404 response both
   produce `AppError::ExternalApi("...")` and are treated identically.

3. **Unstructured logs** — `warn!("acoustid: error for {}: {e}", id)`
   embeds dynamic data in the message string, making log aggregation
   and alerting brittle. Log events have no consistent field schema.

4. **No end-to-end correlation** — a file entering the pipeline at
   the Classifier stage produces dozens of log lines across four crates
   with no shared identifier. Debugging a specific track requires
   grepping for its UUID or path — only possible after fingerprinting.

This pass fixes all four.

---

### Acceptance Criteria

- [ ] `cargo build --workspace` — zero errors, zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] No `Box<dyn std::error::Error>` or `Box<dyn std::error::Error + Send + Sync>`
      as a public return type anywhere in the workspace
- [ ] No bare string errors: `AppError::ExternalApi(String)` is replaced by
      typed variants with structured fields
- [ ] Every `warn!` and `error!` call includes at least one structured field
      (not embedded in the message format string)
- [ ] Every file processed through the pipeline emits a `correlation_id` that
      appears in all log events from Classifier through Tag Writer
- [ ] `LOG_FORMAT=json` produces valid JSON log lines parseable by logstash/loki
- [ ] `LOG_FORMAT=pretty` produces human-readable colored output in dev
- [ ] `is_retryable()` returns the correct value for each error variant
      (verified by unit tests in `crates/application/`)

---

### Scope

| Crate | Change |
|---|---|
| `crates/application/` | **Rewrite** `AppError`; add `Retryable` trait; add `correlation_id` to all event types |
| `crates/adapters-persistence/` | Replace all internal error conversions; structured log fields |
| `crates/adapters-watcher/` | Replace `WatcherError`; instrument with tracing |
| `crates/adapters-media-store/` | Replace `TagReaderError`, `TagWriteError`; instrument |
| `crates/adapters-acoustid/` | Replace string errors with typed variants |
| `crates/adapters-musicbrainz/` | Replace string errors with typed variants |
| `crates/adapters-cover-art/` | Replace string errors with typed variants |
| `crates/shared-config/` | Validate config at startup; emit structured startup errors |
| `apps/bot/` | Add `anyhow` for startup; configure `tracing-subscriber`; structured fields |

**Does NOT add:** new pipeline stages, new crates, new migrations, new Discord
commands.

---

### Part 1 — `AppError` Redesign

Replace the existing flat `AppError` enum in `application/src/error.rs` with
a structured hierarchy using `thiserror`. Every variant must carry enough
context to produce an actionable log message without string interpolation.

```rust
// application/src/error.rs

use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // ── Infrastructure ──────────────────────────────────────────────────

    #[error("database error during {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sqlx::Error,
    },

    #[error("I/O error on {path:?}: {source}")]
    Io {
        path:   Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    // ── Domain ───────────────────────────────────────────────────────────

    #[error("track not found: {id}")]
    TrackNotFound { id: Uuid },

    #[error("album not found: {id}")]
    AlbumNotFound { id: Uuid },

    #[error("duplicate track fingerprint: existing id {existing_id}, \
             attempted location {attempted_location}")]
    DuplicateTrack {
        existing_id:          Uuid,
        attempted_location:   String,
    },

    // ── External APIs ────────────────────────────────────────────────────

    #[error("AcoustID: {kind} — {detail}")]
    AcoustId {
        kind:   AcoustIdErrorKind,
        detail: String,
    },

    #[error("MusicBrainz: {kind} — {detail}")]
    MusicBrainz {
        kind:   MusicBrainzErrorKind,
        detail: String,
    },

    #[error("Cover Art Archive: {kind} — {detail}")]
    CoverArt {
        kind:   CoverArtErrorKind,
        detail: String,
    },

    // ── Pipeline ─────────────────────────────────────────────────────────

    #[error("fingerprint failed for {path:?}: {source}")]
    Fingerprint {
        path:   PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("tag read failed for {path:?}: {source}")]
    TagRead {
        path:   PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("tag write failed for {path:?}: {kind}")]
    TagWrite {
        path:   PathBuf,
        kind:   TagWriteErrorKind,
    },

    // ── Startup / Config ─────────────────────────────────────────────────

    #[error("configuration error — {field}: {message}")]
    Config {
        field:   &'static str,
        message: String,
    },

    #[error("watcher failed to start: {source}")]
    WatcherInit {
        #[source]
        source: notify::Error,
    },
}

// ── Error kind enums (structured, not stringly-typed) ─────────────────────

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum AcoustIdErrorKind {
    #[error("HTTP error")]           HttpError,
    #[error("rate limited")]         RateLimited,
    #[error("invalid response")]     InvalidResponse,
    #[error("service unavailable")]  ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum MusicBrainzErrorKind {
    #[error("not found")]            NotFound,
    #[error("HTTP error")]           HttpError,
    #[error("rate limited")]         RateLimited,
    #[error("invalid response")]     InvalidResponse,
    #[error("service unavailable")]  ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum CoverArtErrorKind {
    #[error("HTTP error")]           HttpError,
    #[error("service unavailable")]  ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum TagWriteErrorKind {
    #[error("no writable tag format found")]  NoTagFormat,
    #[error("rename failed (cross-device)")]  CrossDevice,
    #[error("copy failed")]                   CopyFailed,
    #[error("lofty write failed")]            LoftyError,
}
```

---

### Part 2 — `Retryable` Trait

Define in `application/src/error.rs`. The enrichment state machine uses this
instead of inferring retryability from the error message string.

```rust
use std::time::Duration;

/// Errors that implement this trait carry their own retry policy.
pub trait Retryable {
    /// Whether this error class warrants another enrichment attempt.
    fn is_retryable(&self) -> bool;

    /// Suggested minimum backoff before retry.
    /// None means use the default backoff from Config.
    fn backoff_hint(&self) -> Option<Duration>;
}

impl Retryable for AppError {
    fn is_retryable(&self) -> bool {
        match self {
            // Transient infrastructure failures — always retry
            AppError::Database { source, .. } => is_transient_db(source),
            AppError::Io { .. }               => false, // file errors are permanent

            // API errors — depends on kind
            AppError::AcoustId { kind, .. } => matches!(
                kind,
                AcoustIdErrorKind::RateLimited
                | AcoustIdErrorKind::ServiceUnavailable
            ),
            AppError::MusicBrainz { kind, .. } => matches!(
                kind,
                MusicBrainzErrorKind::RateLimited
                | MusicBrainzErrorKind::ServiceUnavailable
                | MusicBrainzErrorKind::HttpError
            ),
            AppError::CoverArt { kind, .. } => matches!(
                kind,
                CoverArtErrorKind::ServiceUnavailable
            ),

            // Domain errors — not retryable
            AppError::TrackNotFound { .. }
            | AppError::AlbumNotFound { .. }
            | AppError::DuplicateTrack { .. }
            | AppError::Config { .. }     => false,

            // Pipeline errors — not retryable; source must be investigated
            AppError::Fingerprint { .. }
            | AppError::TagRead { .. }
            | AppError::TagWrite { .. }   => false,

            AppError::WatcherInit { .. }  => false,
        }
    }

    fn backoff_hint(&self) -> Option<Duration> {
        match self {
            AppError::AcoustId { kind: AcoustIdErrorKind::RateLimited, .. }
            | AppError::MusicBrainz { kind: MusicBrainzErrorKind::RateLimited, .. } => {
                // Governor handles rate limiting; extra backoff not needed here.
                None
            }
            AppError::MusicBrainz { kind: MusicBrainzErrorKind::ServiceUnavailable, .. }
            | AppError::AcoustId { kind: AcoustIdErrorKind::ServiceUnavailable, .. } => {
                Some(Duration::from_secs(60))
            }
            _ => None,
        }
    }
}

fn is_transient_db(e: &sqlx::Error) -> bool {
    matches!(
        e,
        sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Io(_)
    )
}
```

Update the AcoustID Worker to use `Retryable` instead of comparing attempt counts:

```rust
// In acoustid_worker.rs — replace hardcoded exhaustion logic with:
let retryable = e.is_retryable();
let new_attempts = track.enrichment_attempts + 1;
let status = if !retryable {
    EnrichmentStatus::Failed // permanent: will be retried by attempt-count policy
} else if new_attempts >= self.failed_retry_limit {
    EnrichmentStatus::Exhausted
} else {
    EnrichmentStatus::Failed
};
```

---

### Part 3 — Correlation ID Threading

Add `correlation_id: Uuid` to every pipeline message type in
`application/src/events.rs`. This single UUID identifies one processing
attempt for one file, enabling full end-to-end log correlation.

```rust
// Full updated events.rs

use uuid::Uuid;

fn new_correlation_id() -> Uuid { Uuid::new_v4() }

#[derive(Debug, Clone)]
pub struct TrackScanned {
    pub track_id:        Uuid,
    pub fingerprint:     String,
    pub duration_secs:   u32,
    pub blob_location:   String,
    #[serde(default = "new_correlation_id")]
    pub correlation_id:  Uuid,   // ← generated in Classifier, carried forward
}

#[derive(Debug, Clone)]
pub struct AcoustIdRequest {
    pub track_id:             Uuid,
    pub fingerprint:          String,
    pub duration_secs:        u32,
    pub blob_location:        String,
    pub enrichment_attempts:  i32,
    pub correlation_id:       Uuid,
}

#[derive(Debug, Clone)]
pub struct ToMusicBrainz {
    pub track_id:       Uuid,
    pub mbid:           String,
    pub acoustid_id:    String,
    pub confidence:     f32,
    pub duration_secs:  u32,
    pub blob_location:  String,
    pub correlation_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ToCoverArt {
    pub track_id:       Uuid,
    pub album_id:       Option<Uuid>,
    pub release_mbid:   String,
    pub album_dir:      Option<String>,
    pub blob_location:  String,
    pub correlation_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ToTagWriter {
    pub track_id:       Uuid,
    pub blob_location:  String,
    pub correlation_id: Uuid,
}
```

In the Classifier, generate the correlation_id at the point of emitting
`ToFingerprint` and carry it to `TrackScanned`:

```rust
// classifier.rs — when building ToFingerprint:
let correlation_id = uuid::Uuid::new_v4();
let _ = fp_tx.send(ToFingerprint {
    path, rel, mtime, size_bytes, existing_id,
    correlation_id,
}).await;

// fingerprint.rs — carry through to TrackScanned:
let _ = scan_tx.send(TrackScanned {
    track_id:       inserted.id,
    fingerprint:    fp.fingerprint,
    duration_secs:  fp.duration_secs,
    blob_location:  rel,
    correlation_id: msg.correlation_id,  // ← carried, not regenerated
}).await;
```

The correlation_id must NEVER be regenerated downstream. It is created
once in the Classifier and carried verbatim through every subsequent stage.

---

### Part 4 — Structured Logging Standards

#### 4.1 Field Schema

Every log event must follow this field schema. Fields are structured
key-value pairs, not embedded in the message string.

| Field | Type | When present |
|---|---|---|
| `correlation_id` | Uuid | Whenever available (Classifier onward) |
| `track_id` | Uuid | After DB insert |
| `path` | &str | File path (relative) whenever processing a file |
| `operation` | &str | What the current code is doing |
| `error` | Display | On warn!/error! events |
| `error.kind` | &str | Variant name of the error |
| `retryable` | bool | On enrichment failure events |
| `attempts` | i32 | On enrichment failure events |
| `duration_ms` | u64 | On timed operations |

#### 4.2 Logging Anti-Patterns (prohibited after this pass)

```rust
// WRONG: dynamic data embedded in message string
warn!("acoustid: error for {}: {e}", req.track_id);
info!("fingerprint: indexed new track {} [{rel}]", inserted.id);

// CORRECT: structured fields
warn!(
    track_id = %req.track_id,
    path     = %req.blob_location,
    error    = %e,
    error.kind = e.kind_str(),
    retryable = e.is_retryable(),
    attempts  = attempts,
    operation = "acoustid.lookup",
    "AcoustID lookup failed"
);
info!(
    track_id        = %inserted.id,
    path            = %rel,
    correlation_id  = %msg.correlation_id,
    operation       = "fingerprint.indexed",
    "indexed new track"
);
```

The message string (the last positional argument) must be a static string
that describes what happened — NOT a template with runtime values.

#### 4.3 Add `kind_str()` helpers to `AppError` variants

```rust
impl AppError {
    /// Returns a stable, machine-readable string identifying the error kind.
    /// Used as the `error.kind` structured log field.
    pub fn kind_str(&self) -> &'static str {
        match self {
            AppError::Database { .. }           => "database",
            AppError::Io { .. }                 => "io",
            AppError::TrackNotFound { .. }      => "track_not_found",
            AppError::AlbumNotFound { .. }      => "album_not_found",
            AppError::DuplicateTrack { .. }     => "duplicate_track",
            AppError::AcoustId { kind, .. }     => match kind {
                AcoustIdErrorKind::HttpError         => "acoustid.http_error",
                AcoustIdErrorKind::RateLimited       => "acoustid.rate_limited",
                AcoustIdErrorKind::InvalidResponse   => "acoustid.invalid_response",
                AcoustIdErrorKind::ServiceUnavailable => "acoustid.unavailable",
            },
            AppError::MusicBrainz { kind, .. }  => match kind {
                MusicBrainzErrorKind::NotFound           => "musicbrainz.not_found",
                MusicBrainzErrorKind::HttpError          => "musicbrainz.http_error",
                MusicBrainzErrorKind::RateLimited        => "musicbrainz.rate_limited",
                MusicBrainzErrorKind::InvalidResponse    => "musicbrainz.invalid_response",
                MusicBrainzErrorKind::ServiceUnavailable => "musicbrainz.unavailable",
            },
            AppError::CoverArt { kind, .. }     => match kind {
                CoverArtErrorKind::HttpError          => "cover_art.http_error",
                CoverArtErrorKind::ServiceUnavailable => "cover_art.unavailable",
            },
            AppError::Fingerprint { .. }        => "fingerprint",
            AppError::TagRead { .. }            => "tag_read",
            AppError::TagWrite { .. }           => "tag_write",
            AppError::Config { .. }             => "config",
            AppError::WatcherInit { .. }        => "watcher_init",
        }
    }
}
```

---

### Part 5 — `tracing::instrument` Placement

Instrument the following functions. Use `skip` to avoid logging large
arguments (channel senders, Arc references). Use `fields` to add domain
context. Do not instrument hot inner loops — only entry points.

```rust
// adapters-acoustid/src/lib.rs
#[tracing::instrument(
    name   = "acoustid.lookup",
    skip   = (self),
    fields = (duration_secs = fp.duration_secs),
    err
)]
async fn lookup(&self, fp: &AudioFingerprint) -> Result<Option<AcoustIdMatch>, AppError>

// adapters-musicbrainz/src/lib.rs
#[tracing::instrument(
    name   = "musicbrainz.fetch_recording",
    skip   = (self),
    fields = (mbid = %mbid),
    err
)]
async fn fetch_recording(&self, mbid: &str) -> Result<MbRecording, AppError>

// adapters-media-store/src/tag_reader.rs
#[tracing::instrument(
    name   = "tag_reader.read_file",
    skip_all,
    fields = (path = %path.display()),
    err
)]
pub fn read_file(path: &Path) -> Result<(AudioFingerprint, RawFileTags, u32), AppError>

// adapters-media-store/src/tag_writer.rs
#[tracing::instrument(
    name   = "tag_writer.write_tags_atomic",
    skip   = (tags),
    fields = (path = %path.display()),
    err
)]
pub fn write_tags_atomic(path: &Path, tags: &TagData) -> Result<WriteResult, AppError>

// application/src/enrichment_orchestrator.rs — per-poll-cycle span
tracing::info_span!("enrichment_orchestrator.poll",
    claimed = tracing::field::Empty
)
// After claim: span.record("claimed", claimed.len());

// application/src/acoustid_worker.rs — per-request span
tracing::info_span!("acoustid_worker.process",
    track_id       = %req.track_id,
    correlation_id = %req.correlation_id,
    path           = %req.blob_location,
)

// application/src/musicbrainz_worker.rs
tracing::info_span!("musicbrainz_worker.process",
    track_id       = %msg.track_id,
    correlation_id = %msg.correlation_id,
    mbid           = %msg.mbid,
)

// application/src/cover_art_worker.rs
tracing::info_span!("cover_art_worker.process",
    track_id       = %msg.track_id,
    correlation_id = %msg.correlation_id,
    release_mbid   = %msg.release_mbid,
)

// application/src/tag_writer_worker.rs
tracing::info_span!("tag_writer_worker.process",
    track_id       = %msg.track_id,
    correlation_id = %msg.correlation_id,
    path           = %msg.blob_location,
)
```

#### Span Lifecycle Pattern for Worker Tasks

Each worker `process()` method should use the span explicitly to ensure it
spans the full processing lifecycle including DB writes and channel emits:

```rust
async fn process(&self, msg: ToMusicBrainz) -> Result<(), AppError> {
    let span = tracing::info_span!(
        "musicbrainz_worker.process",
        track_id       = %msg.track_id,
        correlation_id = %msg.correlation_id,
        mbid           = %msg.mbid,
    );
    // Instrument the async block, not just the function call
    async move {
        // ... implementation ...
    }
    .instrument(span)
    .await
}
```

---

### Part 6 — Per-Crate Error Migration Guide

#### `adapters-persistence/`

Replace every:
```rust
.map_err(|e| AppError::Database(e))           // old flat variant
```
with:
```rust
.map_err(|e| AppError::Database {
    operation: "track.insert",    // use a static string describing the query
    source: e,
})
```

Define a helper macro to reduce boilerplate:
```rust
/// Macro: convert sqlx error to AppError::Database with operation context.
macro_rules! db_err {
    ($op:literal) => {
        |e| AppError::Database { operation: $op, source: e }
    };
}

// Usage:
.fetch_one(&self.pool).await.map_err(db_err!("track.find_by_id"))?;
```

#### `adapters-acoustid/`

Map HTTP and parse errors to typed `AppError::AcoustId` variants:

```rust
let resp = self.client.post(...).send().await.map_err(|e| {
    AppError::AcoustId {
        kind:   if e.is_connect() || e.is_timeout() {
                    AcoustIdErrorKind::ServiceUnavailable
                } else {
                    AcoustIdErrorKind::HttpError
                },
        detail: e.to_string(),
    }
})?;

match resp.status().as_u16() {
    200 => { /* continue */ }
    429 => return Err(AppError::AcoustId {
        kind: AcoustIdErrorKind::RateLimited,
        detail: "HTTP 429 Too Many Requests".into(),
    }),
    503 | 502 | 504 => return Err(AppError::AcoustId {
        kind: AcoustIdErrorKind::ServiceUnavailable,
        detail: format!("HTTP {}", resp.status()),
    }),
    _ => return Err(AppError::AcoustId {
        kind: AcoustIdErrorKind::HttpError,
        detail: format!("HTTP {}", resp.status()),
    }),
}

resp.json::<AcoustIdResponse>().await.map_err(|e| AppError::AcoustId {
    kind:   AcoustIdErrorKind::InvalidResponse,
    detail: e.to_string(),
})?
```

Apply the same pattern to `adapters-musicbrainz/` and `adapters-cover-art/`
with their respective kind enums.

#### `adapters-media-store/`

Replace `TagReaderError = Box<dyn std::error::Error + Send + Sync>` entirely:

```rust
// tag_reader.rs — return type changes to:
pub fn read_file(path: &Path) -> Result<(AudioFingerprint, RawFileTags, u32), AppError>

// Internal error handling:
let tagged_file = lofty::read_from_path(path).map_err(|e| AppError::TagRead {
    path: path.to_owned(),
    source: Box::new(e),
})?;

// Symphonia errors:
symphonia::default::get_probe()
    .format(...)
    .map_err(|e| AppError::Fingerprint {
        path: path.to_owned(),
        source: Box::new(e),
    })?;
```

Replace `TagWriteError` uses in `tag_writer.rs`:

```rust
// Rename failures:
std::fs::rename(&temp_path, path).map_err(|e| {
    if e.raw_os_error() == Some(libc::EXDEV) {
        AppError::TagWrite {
            path: path.to_owned(),
            kind: TagWriteErrorKind::CrossDevice,
        }
    } else {
        AppError::TagWrite {
            path: path.to_owned(),
            kind: TagWriteErrorKind::LoftyError,
        }
    }
})?;
```

#### `adapters-watcher/`

Replace `WatcherError` with `AppError::WatcherInit`:

```rust
// watcher.rs
pub fn start(config: Arc<Config>)
    -> Result<(Self, mpsc::Receiver<FileEvent>), AppError>

// Construction:
new_debouncer_opt::<_, PollWatcher>(...)
    .map_err(|e| AppError::WatcherInit { source: e })?;
```

---

### Part 7 — `anyhow` in `apps/bot/`

`anyhow` is used ONLY in the binary crate `apps/bot/`. All library crates
continue to use typed `AppError` with `thiserror`.

Add to `apps/bot/Cargo.toml`:
```toml
anyhow = "1"
```

Change `main()` signature:
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ...
}
```

Use `.context()` and `.with_context()` at every startup step:

```rust
run_migrations(&pool)
    .await
    .context("database migrations failed")?;

let (watcher, file_rx) = MediaWatcher::start(Arc::clone(&config))
    .context("failed to initialize filesystem watcher")?;

let acoustid_key = std::env::var("ACOUSTID_API_KEY")
    .context("ACOUSTID_API_KEY environment variable not set")?;
```

For fatal startup errors, `anyhow` prints a full error chain automatically:
```
Error: failed to initialize filesystem watcher

Caused by:
    0: watcher init error: No such file or directory (os error 2)
    1: MEDIA_ROOT=/mnt/music does not exist or is not accessible
```

---

### Part 8 — `tracing-subscriber` Configuration

Replace the existing subscriber setup in `apps/bot/main.rs` with a
configurable setup that supports both development and production output.

Add to `apps/bot/Cargo.toml`:
```toml
tracing-subscriber = { version = "0.3", features = [
    "env-filter", "fmt", "json", "ansi", "registry"
] }
```

```rust
// apps/bot/src/telemetry.rs

use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,   // dev: colored, multi-line
    Compact,  // staging: single-line, human-readable
    Json,     // prod: machine-parseable
}

impl LogFormat {
    pub fn from_env() -> Self {
        match std::env::var("LOG_FORMAT").as_deref() {
            Ok("json")    => Self::Json,
            Ok("compact") => Self::Compact,
            _             => Self::Pretty,
        }
    }
}

pub fn init_tracing(format: LogFormat) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("teamti=info,warn"));

    let span_events = FmtSpan::CLOSE; // log when spans close (includes duration)

    match format {
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    fmt::layer()
                        .json()
                        .with_span_events(span_events)
                        .with_current_span(true)
                        .with_span_list(false) // reduce verbosity
                )
                .init();
        }
        LogFormat::Compact => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    fmt::layer()
                        .compact()
                        .with_span_events(span_events)
                        .with_ansi(false)
                )
                .init();
        }
        LogFormat::Pretty => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    fmt::layer()
                        .pretty()
                        .with_span_events(span_events)
                )
                .init();
        }
    }
}
```

Call at the very start of `main()`, before any other initialization:
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let format = LogFormat::from_env();
    init_tracing(format);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_format = ?format,
        "TeamTI v2 starting"
    );
    // ...
}
```

---

### Part 9 — `shared-config/` Startup Validation

Add `Config::validate()` that checks all fields with actionable errors:

```rust
// shared-config/src/lib.rs

impl Config {
    /// Validate configuration at startup. Returns all errors at once
    /// so the operator can fix them in one restart cycle.
    pub fn validate(&self) -> Result<(), Vec<AppError>> {
        let mut errors: Vec<AppError> = Vec::new();

        if !self.media_root.exists() {
            errors.push(AppError::Config {
                field:   "MEDIA_ROOT",
                message: format!(
                    "path {:?} does not exist or is not accessible",
                    self.media_root
                ),
            });
        }

        if self.acoustid_api_key.is_empty() {
            errors.push(AppError::Config {
                field:   "ACOUSTID_API_KEY",
                message: "must not be empty".into(),
            });
        }

        if !self.mb_user_agent.contains('/') || !self.mb_user_agent.contains('(') {
            errors.push(AppError::Config {
                field:   "MB_USER_AGENT",
                message: format!(
                    "must be in format 'AppName/version (contact-url)', got {:?}",
                    self.mb_user_agent
                ),
            });
        }

        if self.smb_read_concurrency == 0 {
            errors.push(AppError::Config {
                field:   "SMB_READ_CONCURRENCY",
                message: "must be >= 1".into(),
            });
        }

        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}
```

In `apps/bot/main.rs`:
```rust
let config = Config::from_env().context("failed to load configuration")?;

if let Err(config_errors) = config.validate() {
    for e in &config_errors {
        tracing::error!(
            field = e.field_str(),
            error = %e,
            "configuration validation failed"
        );
    }
    anyhow::bail!("{} configuration error(s) found — see above", config_errors.len());
}
```

---

### Part 10 — Unit Tests for Error Behavior

Add `crates/application/tests/error_tests.rs`:

```rust
#[test]
fn acoustid_rate_limited_is_retryable() {
    let e = AppError::AcoustId {
        kind:   AcoustIdErrorKind::RateLimited,
        detail: "429".into(),
    };
    assert!(e.is_retryable());
}

#[test]
fn musicbrainz_not_found_is_not_retryable() {
    let e = AppError::MusicBrainz {
        kind:   MusicBrainzErrorKind::NotFound,
        detail: "404".into(),
    };
    assert!(!e.is_retryable());
}

#[test]
fn io_error_is_not_retryable() {
    let e = AppError::Io {
        path:   Some(PathBuf::from("/mnt/music/track.mp3")),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "file gone"),
    };
    assert!(!e.is_retryable());
}

#[test]
fn all_errors_have_stable_kind_str() {
    // Ensures kind_str() never panics and returns a non-empty string
    let errors = vec![
        AppError::TrackNotFound { id: Uuid::new_v4() },
        AppError::AcoustId { kind: AcoustIdErrorKind::HttpError, detail: "".into() },
        AppError::MusicBrainz { kind: MusicBrainzErrorKind::NotFound, detail: "".into() },
    ];
    for e in &errors {
        assert!(!e.kind_str().is_empty());
    }
}

#[test]
fn correlation_id_is_carried_not_regenerated() {
    let original = Uuid::new_v4();
    let scanned = TrackScanned {
        track_id:       Uuid::new_v4(),
        fingerprint:    "fp".into(),
        duration_secs:  180,
        blob_location:  "test.mp3".into(),
        correlation_id: original,
    };
    let request = AcoustIdRequest {
        track_id:             scanned.track_id,
        fingerprint:          scanned.fingerprint.clone(),
        duration_secs:        scanned.duration_secs,
        blob_location:        scanned.blob_location.clone(),
        enrichment_attempts:  0,
        correlation_id:       scanned.correlation_id,  // must equal original
    };
    assert_eq!(request.correlation_id, original);
}
```

---

### Workspace `Cargo.toml` additions

```toml
[workspace.dependencies]
anyhow = "1"
```

Add `anyhow = { workspace = true }` to `apps/bot/Cargo.toml` only.

`thiserror` is already a workspace dependency. Verify it is `version = "1"`.
Do not upgrade to `thiserror 2` — it has breaking changes in `#[error]` syntax.

---

### Migration Checklist for the Implementing Agent

Work through each crate in this order. The order matters because later crates
depend on earlier ones having a clean `AppError`.

1. [ ] `crates/application/src/error.rs` — rewrite `AppError` hierarchy
2. [ ] `crates/application/src/events.rs` — add `correlation_id` to all event types
3. [ ] `crates/application/tests/error_tests.rs` — add and verify all tests pass
4. [ ] `crates/adapters-persistence/` — migrate to `db_err!` macro pattern
5. [ ] `crates/adapters-watcher/` — replace `WatcherError` with `AppError::WatcherInit`
6. [ ] `crates/adapters-media-store/` — replace `TagReaderError` and `TagWriteError`
7. [ ] `crates/adapters-acoustid/` — migrate to typed `AcoustIdErrorKind`
8. [ ] `crates/adapters-musicbrainz/` — migrate to typed `MusicBrainzErrorKind`
9. [ ] `crates/adapters-cover-art/` — migrate to typed `CoverArtErrorKind`
10. [ ] `crates/shared-config/` — add `Config::validate()`
11. [ ] `apps/bot/src/telemetry.rs` — add `init_tracing()`; replace existing setup
12. [ ] `apps/bot/src/main.rs` — add `anyhow`, `init_tracing()`, `Config::validate()`
13. [ ] All crates — add `correlation_id` at every `ToFingerprint` construction site
14. [ ] All worker modules — replace bare string log messages with structured fields
15. [ ] All worker modules — add `tracing::instrument` or manual spans per Part 5
16. [ ] Run `grep -rn "Box<dyn std::error::Error" --include="*.rs" .` → expect empty
17. [ ] Run `grep -rn "AppError::ExternalApi\|AppError::Io(" --include="*.rs" .` → expect empty

---

### Invariants (Pass 4.5 Specific)

| Rule | Detail |
|---|---|
| `correlation_id` is never regenerated after the Classifier | Pass through verbatim; `Uuid::new_v4()` appears only in Classifier |
| `kind_str()` returns a stable, dot-namespaced string | Used as a log field; changing it breaks dashboards/alerts |
| `anyhow` appears only in `apps/bot/` | Library crates use `thiserror` + `AppError` only |
| `LOG_FORMAT` defaults to `pretty` when unset | Never default to JSON in dev — unreadable without a log viewer |
| Span `CLOSE` events include timing | `FmtSpan::CLOSE` enables duration tracking per stage |
| Every `warn!` / `error!` call includes `error = %e` | Never omit the error value from error-level log events |

---

### REFERENCE

docs/v2/v2_master.md