use async_trait::async_trait;
use domain::track::TrackSummary;
use domain::user_library::FavouritesPage;
use uuid::Uuid;

use crate::AppError;

#[async_trait]
pub trait UserLibraryPort: Send + Sync {
    // ── Favourites ───────────────────────────────────────────────
    async fn add_favourite(&self, user_id: &str, track_id: Uuid) -> Result<(), AppError>;

    async fn remove_favourite(&self, user_id: &str, track_id: Uuid) -> Result<(), AppError>;

    async fn is_favourite(&self, user_id: &str, track_id: Uuid) -> Result<bool, AppError>;

    async fn list_favourites(
        &self,
        user_id: &str,
        page: i64,
        page_size: i64,
    ) -> Result<FavouritesPage, AppError>;

    // ── Listen history ───────────────────────────────────────────
    async fn open_listen_event(
        &self,
        user_id: &str,
        track_id: Uuid,
        guild_id: &str,
    ) -> Result<Uuid, AppError>; // returns listen_event id

    async fn close_dangling_events(&self, older_than_secs: i64) -> Result<u64, AppError>;

    async fn close_listen_event(
        &self,
        user_id: &str,
        track_id: Uuid,
        play_duration_ms: i32,
        track_duration_ms: i32,
    ) -> Result<(), AppError>;
    // Internally computes completed = play_duration_ms / track_duration_ms >= THRESHOLD

    /// Close all open listen events for a specific track in a guild.
    /// Used when a track ends, is skipped, or the bot disconnects.
    async fn close_listen_events_for_track(
        &self,
        track_id: Uuid,
        guild_id: &str,
        play_duration_ms: i32,
        track_duration_ms: i32,
    ) -> Result<u64, AppError>;

    async fn recent_history(
        &self,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<TrackSummary>, AppError>;
}
