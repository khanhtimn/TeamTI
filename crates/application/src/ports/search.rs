use async_trait::async_trait;
use domain::error::DomainError;
use uuid::Uuid;

#[async_trait]
pub trait MediaSearchPort: Send + Sync {
    /// Search media assets by title/artist/filename.
    /// Returns up to `limit` results ordered by relevance.
    async fn search_assets(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MediaSearchResult>, DomainError>;
}

pub struct MediaSearchResult {
    pub asset_id: Uuid,
    pub title: String,
    pub artist: Option<String>,
    pub original_filename: Option<String>,
}
