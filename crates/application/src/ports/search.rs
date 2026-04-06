use crate::AppError;
use async_trait::async_trait;
use domain::track::TrackSummary;
use uuid::Uuid;

#[async_trait]
pub trait TrackSearchPort: Send + Sync {
    async fn autocomplete(&self, query: &str, limit: usize) -> Result<Vec<TrackSummary>, AppError>;

    /// Full index rebuild from PostgreSQL source of truth.
    async fn rebuild_index(&self) -> Result<usize, AppError>;

    /// Reindex a single track by UUID after enrichment completes.
    async fn reindex_track(&self, track_id: Uuid) -> Result<(), AppError>;
}
