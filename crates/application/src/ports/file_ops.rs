use crate::AppError;
use async_trait::async_trait;
use std::path::Path;

/// Enriched metadata to write back into file tags.
#[derive(Debug, Clone)]
pub struct EnrichedTags {
    pub title: String,
    pub artist_display: String,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub cover_art: Option<bytes::Bytes>,
}

/// Result after writing tags back to file.
#[derive(Debug, Clone)]
pub struct TagWriteResult {
    pub new_file_modified_at: chrono::DateTime<chrono::Utc>,
    pub new_file_size_bytes: i64,
}

#[async_trait]
pub trait FileTagWriterPort: Send + Sync {
    /// Write enriched tags to the file at `absolute_path` atomically
    /// (tempfile + rename). Returns new mtime and size after write.
    async fn write_tags(
        &self,
        absolute_path: &Path,
        tags: &EnrichedTags,
    ) -> Result<TagWriteResult, AppError>;
}
