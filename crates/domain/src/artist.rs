use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Artist {
    pub id: Uuid,
    pub name: String,
    pub sort_name: String,
    pub mbid: Option<String>,
    pub country: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Role of an artist credited on an album.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ArtistRole {
    #[default]
    Primary,
    Various,
    Compiler,
    Featuring,
    Remixer,
    Producer,
}

/// Join record: artist credited on an album.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AlbumArtist {
    pub album_id: Uuid,
    pub artist_id: Uuid,
    pub role: ArtistRole,
    pub position: i32,
}

/// Join record: artist credited on a track.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TrackArtist {
    pub track_id: Uuid,
    pub artist_id: Uuid,
    pub role: ArtistRole,
    pub position: i32,
}
