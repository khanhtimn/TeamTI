use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Album {
    pub id: Uuid,
    pub title: String,
    pub release_year: Option<i32>,
    pub total_tracks: Option<i32>,
    pub total_discs: Option<i32>,
    pub mbid: Option<String>,
    /// Path relative to MEDIA_ROOT, e.g. "Artist/Album/cover.jpg"
    pub cover_art_path: Option<String>,
    pub created_at: DateTime<Utc>,
}
