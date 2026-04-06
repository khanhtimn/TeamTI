use async_trait::async_trait;
use domain::analysis::MoodWeight;
use domain::track::TrackSummary;
use uuid::Uuid;

use crate::AppError;

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait RecommendationPort: Send + Sync {
    /// Generate a ranked list of recommended tracks for a user.
    /// Used for radio refill and /play empty-query suggestions.
    ///
    /// - `seed_track_id`: current playing track (radio context).
    ///   If `None`, derive seed purely from user profile.
    /// - `seed_vector`: bliss vector of the seed track (if available).
    /// - `user_centroid`: weighted average of user's liked track vectors.
    /// - `mood_weight`: acoustic vs taste bias.
    /// - `exclude`: track IDs already in the queue (do not repeat).
    /// - `limit`: how many tracks to return.
    async fn recommend(
        &self,
        user_id: &str,
        seed_track_id: Option<Uuid>,
        seed_vector: Option<Vec<f32>>,
        mood_weight: MoodWeight,
        exclude: &[Uuid],
        limit: usize,
    ) -> Result<Vec<TrackSummary>, AppError>;

    /// Recompute top-K track affinities for a user and write to
    /// `user_track_affinities`. Called after listen completion + favourite add.
    async fn refresh_affinities(&self, user_id: &str, limit: usize) -> Result<(), AppError>;

    /// Update `user_genre_stats` for a completed listen.
    async fn update_genre_stats(&self, user_id: &str, genres: &[String]) -> Result<(), AppError>;

    /// Update `guild_track_stats` for a completed listen.
    async fn update_guild_track_stats(
        &self,
        guild_id: &str,
        track_id: Uuid,
    ) -> Result<(), AppError>;

    /// Fetch the bliss vector for a track. Returns None if not yet analysed.
    async fn get_bliss_vector(&self, track_id: Uuid) -> Result<Option<Vec<f32>>, AppError>;

    /// Compute user's acoustic centroid vector (weighted avg of liked track vectors).
    async fn compute_user_centroid(&self, user_id: &str) -> Result<Option<Vec<f32>>, AppError>;
}
