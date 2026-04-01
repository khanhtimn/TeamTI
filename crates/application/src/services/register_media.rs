use std::sync::Arc;
use std::path::Path;
use domain::error::DomainError;
use domain::media::MediaAsset;
use crate::ports::media_repository::MediaRepository;
use crate::ports::media_store::MediaStore;
use crate::dto::LocalMediaRegistrationResult;

pub struct RegisterMedia {
    metadata_repo: Arc<dyn MediaRepository>,
    store: Arc<dyn MediaStore>,
}

impl RegisterMedia {
    pub fn new(metadata_repo: Arc<dyn MediaRepository>, store: Arc<dyn MediaStore>) -> Self {
        Self { metadata_repo, store }
    }

    pub async fn execute_local(&self, path: &str) -> Result<LocalMediaRegistrationResult, DomainError> {
        // Import file into managed store
        let (_blob_ref, origin) = self.store.import_local(path).await?;
        
        // Derive basic title
        let p = Path::new(path);
        let title = p.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown Local File")
            .to_string();

        let asset = MediaAsset {
            id: uuid::Uuid::new_v4(),
            title: title.clone(),
            origin,
            duration_ms: None, // To be extracted eventually using symphonia
        };

        // Save metadata
        self.metadata_repo.save(&asset).await?;

        Ok(LocalMediaRegistrationResult {
            asset_id: asset.id,
            title,
        })
    }
}
