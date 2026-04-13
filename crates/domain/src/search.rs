use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub source: String,
    pub track_id: Option<Uuid>,
    pub youtube_video_id: Option<String>,
    pub title: String,
    pub artist_display: Option<String>,
    pub uploader: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchFilter {
    All,
    YoutubeOnly,
    LocalOnly,
}
