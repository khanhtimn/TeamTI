use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::track::TrackSummary;

// ── Favourites ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Favorite {
    pub id: Uuid,
    /// Discord user snowflake stored as string
    pub user_id: String,
    pub track_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct FavouritesPage {
    pub tracks: Vec<TrackSummary>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

// ── Listen Events ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ListenEvent {
    pub id: Uuid,
    pub user_id: String,
    pub track_id: Uuid,
    pub guild_id: String,
    pub started_at: DateTime<Utc>,
    /// NULL until the event is closed (track ends, skipped, or bot leaves vc).
    /// Set to elapsed playback time, not wall time.
    pub play_duration_ms: Option<i64>,
    /// Computed at event close: play_duration_ms / tracks.duration_ms >= 0.8
    pub completed: bool,
}

// ── Playlists ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistVisibility {
    Private,
    Public,
}

impl PlaylistVisibility {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}

impl std::str::FromStr for PlaylistVisibility {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "private" => Ok(Self::Private),
            "public" => Ok(Self::Public),
            other => Err(format!("invalid playlist visibility: {other}")),
        }
    }
}

impl std::fmt::Display for PlaylistVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: Uuid,
    pub name: String,
    pub owner_id: String,
    pub visibility: PlaylistVisibility,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PlaylistSummary {
    pub id: Uuid,
    pub name: String,
    pub owner_id: String,
    pub visibility: PlaylistVisibility,
    pub track_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub id: Uuid,
    pub playlist_id: Uuid,
    pub track_id: Uuid,
    pub position: i32,
    pub added_by: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PlaylistPage {
    pub items: Vec<(PlaylistItem, TrackSummary)>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}
