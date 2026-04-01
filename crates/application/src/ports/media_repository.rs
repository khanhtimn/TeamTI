use async_trait::async_trait;
use domain::media::MediaAsset;
use domain::error::DomainError;

#[async_trait]
pub trait MediaRepository: Send + Sync {
    async fn save(&self, asset: &MediaAsset) -> Result<(), DomainError>;
    async fn find_by_id(&self, id: uuid::Uuid) -> Result<Option<MediaAsset>, DomainError>;
    async fn find_by_content_hash(&self, hash: &str) -> Result<Option<MediaAsset>, DomainError>;
    async fn search(&self, query: &str, limit: i64) -> Result<Vec<MediaAsset>, DomainError>;
}
