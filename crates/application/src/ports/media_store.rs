use async_trait::async_trait;
use domain::media::{ManagedBlobRef, PlayableSource};
use domain::error::DomainError;

#[async_trait]
pub trait MediaStore: Send + Sync {
    /// Resolves an asset's storage location into a playable source.
    async fn resolve_playable(&self, blob_ref: &ManagedBlobRef) -> Result<PlayableSource, DomainError>;
    
    /// Imports a file path into the managed media store.
    async fn import_local(&self, source_path: &str) -> Result<(ManagedBlobRef, domain::media::MediaOrigin), DomainError>;
}
