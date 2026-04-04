use std::fmt;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

// ── Opaque infrastructure error types ─────────────────────────────────────
// The application layer classifies infrastructure failures by *kind* without
// depending on concrete driver types (sqlx, notify, etc.). Conversion from
// concrete errors lives in the adapter crates.

/// Classifies persistence failures without exposing the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    /// Connection pool exhausted (e.g. sqlx::Error::PoolTimedOut)
    PoolExhausted,
    /// Connection lost or I/O error on a DB connection
    ConnectionLost,
    /// Violated a unique/FK constraint
    ConstraintViolation,
    /// Row not found (SELECT returned no results unexpectedly)
    NotFound,
    /// Any other DB error
    Other,
}

impl fmt::Display for PersistenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistenceKind::PoolExhausted => write!(f, "pool_exhausted"),
            PersistenceKind::ConnectionLost => write!(f, "connection_lost"),
            PersistenceKind::ConstraintViolation => write!(f, "constraint_violation"),
            PersistenceKind::NotFound => write!(f, "not_found"),
            PersistenceKind::Other => write!(f, "other"),
        }
    }
}

/// Opaque persistence error — no sqlx types exposed.
#[derive(Debug)]
pub struct PersistenceError {
    pub operation: &'static str,
    pub kind: PersistenceKind,
    pub message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl PersistenceError {
    pub fn new(
        operation: &'static str,
        kind: PersistenceKind,
        message: impl Into<String>,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            operation,
            kind,
            message: message.into(),
            source,
        }
    }

    /// Whether this persistence error is transient and warrants retry.
    pub fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            PersistenceKind::PoolExhausted | PersistenceKind::ConnectionLost
        )
    }
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "persistence error during {}: {} — {}",
            self.operation, self.kind, self.message
        )
    }
}

impl std::error::Error for PersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// Opaque watcher initialization error — no notify types exposed.
#[derive(Debug)]
pub struct WatcherError {
    pub message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WatcherError {
    pub fn new(
        message: impl Into<String>,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            message: message.into(),
            source,
        }
    }
}

impl fmt::Display for WatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "watcher error: {}", self.message)
    }
}

impl std::error::Error for WatcherError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

// ── Main error enum ───────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // ── Infrastructure (opaque) ──────────────────────────────────────────
    #[error(transparent)]
    Persistence(#[from] PersistenceError),

    #[error("I/O error on {path:?}: {source}")]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    // ── Domain ───────────────────────────────────────────────────────────
    #[error("track not found: {id}")]
    TrackNotFound { id: Uuid },

    #[error("album not found: {id}")]
    AlbumNotFound { id: Uuid },

    #[error(
        "duplicate track fingerprint: existing id {existing_id}, \
             attempted location {attempted_location}"
    )]
    DuplicateTrack {
        existing_id: Uuid,
        attempted_location: String,
    },

    // ── External APIs ────────────────────────────────────────────────────
    #[error("Voice error: {kind} — {detail}")]
    Voice {
        kind: VoiceErrorKind,
        detail: String,
    },

    #[error("AcoustID: {kind} — {detail}")]
    AcoustId {
        kind: AcoustIdErrorKind,
        detail: String,
    },

    #[error("MusicBrainz: {kind} — {detail}")]
    MusicBrainz {
        kind: MusicBrainzErrorKind,
        detail: String,
    },

    #[error("Cover Art Archive: {kind} — {detail}")]
    CoverArt {
        kind: CoverArtErrorKind,
        detail: String,
    },

    #[error("LRCLIB: {kind} — {detail}")]
    LrcLib {
        kind: LrcLibErrorKind,
        detail: String,
    },

    // ── Pipeline ─────────────────────────────────────────────────────────
    #[error("fingerprint failed for {path:?}: {source}")]
    Fingerprint {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("tag read failed for {path:?}: {source}")]
    TagRead {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("tag write failed for {path:?}: {kind}")]
    TagWrite {
        path: PathBuf,
        kind: TagWriteErrorKind,
    },

    // ── Startup / Config ─────────────────────────────────────────────────
    #[error("configuration error — {field}: {message}")]
    Config {
        field: &'static str,
        message: String,
    },

    #[error(transparent)]
    WatcherInit(#[from] WatcherError),
}

// ── Error kind enums (structured, not stringly-typed) ─────────────────────

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum VoiceErrorKind {
    #[error("Songbird not initialized")]
    NotInitialized,
    #[error("failed to join channel")]
    JoinFailed,
    #[error("not in a voice channel")]
    NotInChannel,
    #[error("audio file not found")]
    FileNotFound,
    #[error("track decode error")]
    DecodeError,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum AcoustIdErrorKind {
    #[error("HTTP error")]
    HttpError,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid response")]
    InvalidResponse,
    #[error("service unavailable")]
    ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum MusicBrainzErrorKind {
    #[error("not found")]
    NotFound,
    #[error("HTTP error")]
    HttpError,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid response")]
    InvalidResponse,
    #[error("service unavailable")]
    ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum CoverArtErrorKind {
    #[error("HTTP error")]
    HttpError,
    #[error("service unavailable")]
    ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum LrcLibErrorKind {
    #[error("not found")]
    NotFound,
    #[error("HTTP error")]
    HttpError,
    #[error("rate limited")]
    RateLimited,
    #[error("invalid response")]
    InvalidResponse,
    #[error("service unavailable")]
    ServiceUnavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum TagWriteErrorKind {
    #[error("no writable tag format found")]
    NoTagFormat,
    #[error("rename failed (cross-device)")]
    CrossDevice,
    #[error("copy failed")]
    CopyFailed,
    #[error("lofty write failed")]
    LoftyError,
}

impl AppError {
    /// Returns a stable, machine-readable string identifying the error kind.
    /// Used as the `error.kind` structured log field.
    pub fn kind_str(&self) -> &'static str {
        match self {
            AppError::Persistence(e) => match e.kind {
                PersistenceKind::PoolExhausted => "persistence.pool_exhausted",
                PersistenceKind::ConnectionLost => "persistence.connection_lost",
                PersistenceKind::ConstraintViolation => "persistence.constraint_violation",
                PersistenceKind::NotFound => "persistence.not_found",
                PersistenceKind::Other => "persistence.other",
            },
            AppError::Io { .. } => "io",
            AppError::TrackNotFound { .. } => "track_not_found",
            AppError::AlbumNotFound { .. } => "album_not_found",
            AppError::DuplicateTrack { .. } => "duplicate_track",
            AppError::Voice { kind, .. } => match kind {
                VoiceErrorKind::NotInitialized => "voice.not_initialized",
                VoiceErrorKind::JoinFailed => "voice.join_failed",
                VoiceErrorKind::NotInChannel => "voice.not_in_channel",
                VoiceErrorKind::FileNotFound => "voice.file_not_found",
                VoiceErrorKind::DecodeError => "voice.decode_error",
            },
            AppError::AcoustId { kind, .. } => match kind {
                AcoustIdErrorKind::HttpError => "acoustid.http_error",
                AcoustIdErrorKind::RateLimited => "acoustid.rate_limited",
                AcoustIdErrorKind::InvalidResponse => "acoustid.invalid_response",
                AcoustIdErrorKind::ServiceUnavailable => "acoustid.unavailable",
            },
            AppError::MusicBrainz { kind, .. } => match kind {
                MusicBrainzErrorKind::NotFound => "musicbrainz.not_found",
                MusicBrainzErrorKind::HttpError => "musicbrainz.http_error",
                MusicBrainzErrorKind::RateLimited => "musicbrainz.rate_limited",
                MusicBrainzErrorKind::InvalidResponse => "musicbrainz.invalid_response",
                MusicBrainzErrorKind::ServiceUnavailable => "musicbrainz.unavailable",
            },
            AppError::CoverArt { kind, .. } => match kind {
                CoverArtErrorKind::HttpError => "cover_art.http_error",
                CoverArtErrorKind::ServiceUnavailable => "cover_art.unavailable",
            },
            AppError::LrcLib { kind, .. } => match kind {
                LrcLibErrorKind::NotFound => "lrclib.not_found",
                LrcLibErrorKind::HttpError => "lrclib.http_error",
                LrcLibErrorKind::RateLimited => "lrclib.rate_limited",
                LrcLibErrorKind::InvalidResponse => "lrclib.invalid_response",
                LrcLibErrorKind::ServiceUnavailable => "lrclib.unavailable",
            },
            AppError::Fingerprint { .. } => "fingerprint",
            AppError::TagRead { .. } => "tag_read",
            AppError::TagWrite { .. } => "tag_write",
            AppError::Config { .. } => "config",
            AppError::WatcherInit { .. } => "watcher_init",
        }
    }
}

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
            // Transient infrastructure failures — retry if pool/connection issue
            AppError::Persistence(e) => e.is_transient(),
            AppError::Io { .. } => false, // file errors are permanent

            // API errors — depends on kind
            AppError::Voice { kind, .. } => matches!(kind, VoiceErrorKind::JoinFailed),
            AppError::AcoustId { kind, .. } => matches!(
                kind,
                AcoustIdErrorKind::RateLimited | AcoustIdErrorKind::ServiceUnavailable
            ),
            AppError::MusicBrainz { kind, .. } => matches!(
                kind,
                MusicBrainzErrorKind::RateLimited
                    | MusicBrainzErrorKind::ServiceUnavailable
                    | MusicBrainzErrorKind::HttpError
            ),
            AppError::CoverArt { kind, .. } => {
                matches!(kind, CoverArtErrorKind::ServiceUnavailable)
            }

            AppError::LrcLib { kind, .. } => matches!(
                kind,
                LrcLibErrorKind::RateLimited
                    | LrcLibErrorKind::HttpError
                    | LrcLibErrorKind::ServiceUnavailable
            ),

            // Domain errors — not retryable
            AppError::TrackNotFound { .. }
            | AppError::AlbumNotFound { .. }
            | AppError::DuplicateTrack { .. }
            | AppError::Config { .. } => false,

            // Pipeline errors — not retryable; source must be investigated
            AppError::Fingerprint { .. } | AppError::TagRead { .. } | AppError::TagWrite { .. } => {
                false
            }

            AppError::WatcherInit { .. } => false,
        }
    }

    fn backoff_hint(&self) -> Option<Duration> {
        match self {
            AppError::AcoustId {
                kind: AcoustIdErrorKind::RateLimited,
                ..
            }
            | AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::RateLimited,
                ..
            } => {
                // Governor handles rate limiting; extra backoff not needed here.
                None
            }
            AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::ServiceUnavailable,
                ..
            }
            | AppError::Voice {
                kind: VoiceErrorKind::JoinFailed,
                ..
            } => Some(Duration::from_secs(5)),
            AppError::AcoustId {
                kind: AcoustIdErrorKind::ServiceUnavailable,
                ..
            } => Some(Duration::from_secs(60)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retryable_persistence() {
        let err = AppError::Persistence(PersistenceError::new(
            "test",
            PersistenceKind::PoolExhausted,
            "pool timed out",
            None,
        ));
        assert!(err.is_retryable());

        let err = AppError::Persistence(PersistenceError::new(
            "test",
            PersistenceKind::NotFound,
            "row not found",
            None,
        ));
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_retryable_cover_art() {
        let err = AppError::CoverArt {
            kind: CoverArtErrorKind::HttpError,
            detail: "404".into(),
        };
        assert!(!err.is_retryable());

        let err = AppError::CoverArt {
            kind: CoverArtErrorKind::ServiceUnavailable,
            detail: "502".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn test_retryable_acoustid() {
        let err = AppError::AcoustId {
            kind: AcoustIdErrorKind::RateLimited,
            detail: "429".into(),
        };
        assert!(err.is_retryable());

        let err = AppError::AcoustId {
            kind: AcoustIdErrorKind::InvalidResponse,
            detail: "bad json".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_persistence_kind_display() {
        assert_eq!(PersistenceKind::PoolExhausted.to_string(), "pool_exhausted");
        assert_eq!(
            PersistenceKind::ConnectionLost.to_string(),
            "connection_lost"
        );
    }
}
