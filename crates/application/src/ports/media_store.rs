use async_trait::async_trait;
use domain::media::{ManagedBlobRef, PlayableSource};
use domain::error::DomainError;

#[async_trait]
pub trait MediaStore: Send + Sync {
    /// Resolves an asset's storage location into a playable source.
    async fn resolve_playable(&self, blob_ref: &ManagedBlobRef) -> Result<PlayableSource, DomainError>;
}
