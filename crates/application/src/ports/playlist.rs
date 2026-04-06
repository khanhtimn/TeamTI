use async_trait::async_trait;
use domain::track::TrackSummary;
use domain::user_library::{
    Playlist, PlaylistItem, PlaylistPage, PlaylistSummary, PlaylistVisibility,
};
use uuid::Uuid;

use crate::AppError;

#[async_trait]
pub trait PlaylistPort: Send + Sync {
    // ── Playlist CRUD ────────────────────────────────────────────
    async fn create_playlist(
        &self,
        owner_id: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<Playlist, AppError>;

    async fn rename_playlist(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        new_name: &str,
    ) -> Result<(), AppError>;

    async fn delete_playlist(&self, playlist_id: Uuid, owner_id: &str) -> Result<(), AppError>;

    async fn set_visibility(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        visibility: PlaylistVisibility,
    ) -> Result<(), AppError>;

    // ── Items ────────────────────────────────────────────────────
    async fn add_track(
        &self,
        playlist_id: Uuid,
        track_id: Uuid,
        added_by: &str,
    ) -> Result<PlaylistItem, AppError>;

    async fn remove_track(
        &self,
        playlist_id: Uuid,
        item_id: Uuid,
        requesting_user: &str,
    ) -> Result<(), AppError>;

    async fn reorder_track(
        &self,
        playlist_id: Uuid,
        item_id: Uuid,
        new_position: i32,
        requesting_user: &str,
    ) -> Result<(), AppError>;

    // ── Queries ──────────────────────────────────────────────────
    async fn list_user_playlists(&self, owner_id: &str) -> Result<Vec<PlaylistSummary>, AppError>;

    /// List playlists accessible to a user (own + public from others).
    /// Used for autocomplete on read operations.
    async fn list_accessible_playlists(
        &self,
        user_id: &str,
    ) -> Result<Vec<PlaylistSummary>, AppError>;

    async fn get_playlist_items(
        &self,
        playlist_id: Uuid,
        requesting_user: &str,
        page: i64,
        page_size: i64,
    ) -> Result<PlaylistPage, AppError>;

    async fn get_playlist_tracks(
        &self,
        playlist_id: Uuid,
        requesting_user: &str,
    ) -> Result<Vec<TrackSummary>, AppError>;

    // ── Collaboration ────────────────────────────────────────────
    async fn add_collaborator(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        new_collaborator_id: &str,
    ) -> Result<(), AppError>;

    async fn remove_collaborator(
        &self,
        playlist_id: Uuid,
        owner_id: &str,
        collaborator_id: &str,
    ) -> Result<(), AppError>;

    async fn list_collaborators(
        &self,
        playlist_id: Uuid,
        requesting_user: &str,
    ) -> Result<Vec<String>, AppError>; // returns user IDs
}
