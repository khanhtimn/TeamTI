use chrono::{DateTime, Utc};

/// Metadata extracted from yt-dlp --dump-json output.
#[derive(Debug, Clone)]
pub struct VideoMetadata {
    pub video_id: String,
    pub url: String,
    pub title: Option<String>,
    /// YouTube channel name. None for flat-playlist entries.
    pub uploader: Option<String>,
    pub channel_id: Option<String>,
    /// Duration in milliseconds (from yt-dlp "duration" seconds × 1000).
    pub duration_ms: Option<i64>,
    pub thumbnail_url: Option<String>,
    /// Sometimes populated for music videos uploaded by labels.
    pub track_title: Option<String>,
    /// "artist" field in yt-dlp JSON.
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Row model for `youtube_download_jobs` table.
#[derive(Debug, Clone)]
pub struct YoutubeDownloadJob {
    pub video_id: String,
    pub track_id: uuid::Uuid,
    pub url: String,
    pub status: String,
    pub attempts: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Insert DTO for creating a new download job.
#[derive(Debug, Clone)]
pub struct NewYoutubeDownloadJob {
    pub video_id: String,
    pub track_id: uuid::Uuid,
    pub url: String,
}
