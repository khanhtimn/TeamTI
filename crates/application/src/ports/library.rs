use crate::AppError;
use async_trait::async_trait;
use domain::{ListenEvent, Playlist, PlaylistItem, track::TrackSummary};
use uuid::Uuid;

pub enum FavoriteStatus {
    Added,
    Removed,
}

#[async_trait]
pub trait LibraryQueryPort: Send + Sync {
    async fn get_favorites(&self, user_id: &str) -> Result<Vec<TrackSummary>, AppError>;

    async fn toggle_favorite(
        &self,
        user_id: &str,
        track_id: Uuid,
    ) -> Result<FavoriteStatus, AppError>;

    async fn record_listen(&self, event: &ListenEvent) -> Result<(), AppError>;

    async fn get_listen_history(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<ListenEvent>, AppError>;

    async fn create_playlist(&self, owner_id: &str, name: &str) -> Result<Playlist, AppError>;

    async fn add_to_playlist(
        &self,
        playlist_id: Uuid,
        track_id: Uuid,
    ) -> Result<PlaylistItem, AppError>;

    async fn get_playlist_tracks(&self, playlist_id: Uuid) -> Result<Vec<TrackSummary>, AppError>;

    async fn get_user_playlists(&self, owner_id: &str) -> Result<Vec<Playlist>, AppError>;
}
