use std::path::Path;

use async_trait::async_trait;

use crate::AppError;
use domain::VideoMetadata;

#[async_trait]
pub trait YtDlpPort: Send + Sync {
    /// Fetch metadata for a single video (no download).
    /// Provides stream URL valid for ~6 hours.
    async fn fetch_video_metadata(&self, url: &str) -> Result<VideoMetadata, AppError>;

    /// Fetch flat playlist metadata (no per-video format fetch).
    /// Returns one entry per playlist item — fast for any playlist size.
    async fn fetch_playlist_metadata(&self, url: &str) -> Result<Vec<VideoMetadata>, AppError>;

    /// Search YouTube and return the top result's metadata.
    async fn search_top_result(&self, query: &str) -> Result<Option<VideoMetadata>, AppError>;

    /// Download audio for a single video to the given absolute path.
    /// Start a subprocess to download audio (Opus format expected).
    async fn download_audio(&self, url: &str, output_path: &Path) -> Result<(), AppError>;

    /// Compute the blob path for a YouTube video.
    fn compute_blob_path(&self, uploader: &str, title: &str, video_id: &str) -> String;

    /// Search YouTube and return top N results as metadata.
    /// Internally uses "ytsearch{n}:{query}".
    async fn search_top_n(&self, query: &str, n: usize) -> Result<Vec<VideoMetadata>, AppError>;
}
