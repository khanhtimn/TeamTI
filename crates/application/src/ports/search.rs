use crate::AppError;
use async_trait::async_trait;
use domain::search::{SearchFilter, SearchResult};
use uuid::Uuid;

#[async_trait]
pub trait MusicSearchPort: Send + Sync {
    async fn autocomplete(
        &self,
        query: &str,
        filter: SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>, AppError>;

    /// Full index rebuild from PostgreSQL source of truth.
    async fn rebuild_index(&self) -> Result<usize, AppError>;

    /// Reindex a single track by UUID after enrichment completes.
    async fn reindex_track(&self, track_id: Uuid) -> Result<(), AppError>;

    /// Index a batch of YouTube search results directly.
    /// Deduplicates internally if the video_id already exists in the Tracks schema.
    async fn add_search_results(&self, results: Vec<SearchResult>) -> Result<(), AppError>;

    /// Delete a search result from the index by video_id if it exists.
    /// Used when a search result is promoted to a track.
    async fn delete_search_result(&self, video_id: &str) -> Result<(), AppError>;
}
