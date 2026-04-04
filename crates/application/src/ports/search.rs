use crate::AppError;
use async_trait::async_trait;
use domain::track::TrackSummary;

#[async_trait]
pub trait TrackSearchPort: Send + Sync {
    /// Hybrid FTS + trigram search. Only returns tracks with
    /// `enrichment_status` = 'done'.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<TrackSummary>, AppError>;

    /// Autocomplete: prefix match on `title` and `artist_display`.
    /// Only returns tracks with `enrichment_status` = 'done'.
    async fn autocomplete(&self, prefix: &str, limit: usize)
    -> Result<Vec<TrackSummary>, AppError>;
}
