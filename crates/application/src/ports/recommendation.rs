use async_trait::async_trait;
use domain::track::TrackSummary;
use uuid::Uuid;

use crate::AppError;

#[async_trait]
pub trait RecommendationPort: Send + Sync {
    /// Generate a ranked list of recommended tracks for a user.
    /// Used for radio refill and /play empty-query suggestions.
    ///
    /// `seed_track_id`: current playing track (radio context).
    ///   If `None`, derive seed purely from user profile.
    /// `exclude`: track IDs already in the queue (do not repeat).
    /// `limit`: how many tracks to return.
    async fn recommend(
        &self,
        user_id: &str,
        seed_track_id: Option<Uuid>,
        exclude: &[Uuid],
        limit: usize,
    ) -> Result<Vec<TrackSummary>, AppError>;
}
