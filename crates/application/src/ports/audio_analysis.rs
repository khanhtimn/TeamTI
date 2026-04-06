use async_trait::async_trait;

use crate::AppError;

#[async_trait]
pub trait AudioAnalysisPort: Send + Sync {
    /// Analyse a track and return its bliss feature vector.
    /// `blob_location` is relative to MEDIA_ROOT.
    /// Returns `AppError::Analysis` on file-not-found or decode failure.
    async fn analyse_track(&self, blob_location: &str) -> Result<Vec<f32>, AppError>;
}
