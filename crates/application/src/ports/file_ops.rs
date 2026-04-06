use crate::AppError;
use async_trait::async_trait;

/// Metadata to write back into file tags.
/// All fields are Clone + Send — safe to move into spawn_blocking.
#[derive(Debug, Clone)]
pub struct TagData {
    pub title: String,
    pub artist: String,
    pub album_title: Option<String>,
    pub year: Option<i32>,
    pub genres: Vec<String>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    // Extended metadata
    pub bpm: Option<i32>,
    pub isrc: Option<String>,
    pub composers: Vec<String>,
    pub lyricists: Vec<String>,
    pub lyrics: Option<String>,
}

/// Result after writing tags back to file.
#[derive(Debug, Clone)]
pub struct WriteResult {
    pub new_mtime: chrono::DateTime<chrono::Utc>,
    pub new_size_bytes: i64,
}

/// A1 fix: The port owns SMB semaphore acquisition and `spawn_blocking`
/// internally. The worker never acquires the semaphore — it delegates to
/// the port entirely, preventing double-permit acquisition.
#[async_trait]
pub trait FileTagWriterPort: Send + Sync {
    /// Write enriched tags to the file atomically (copy → modify → rename).
    /// `blob_location` is relative to `MEDIA_ROOT`; the port joins with its
    /// root to get the absolute path.
    async fn write_tags(
        &self,
        blob_location: &str,
        tags: &TagData,
    ) -> Result<WriteResult, AppError>;
}
