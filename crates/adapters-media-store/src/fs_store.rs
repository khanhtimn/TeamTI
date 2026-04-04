use domain::error::DomainError;
use domain::media::PlayableSource;
use std::path::PathBuf;

pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl AsRef<std::path::Path>) -> Self {
        std::fs::create_dir_all(root.as_ref()).ok();
        let canonical =
            std::fs::canonicalize(root.as_ref()).unwrap_or_else(|_| root.as_ref().to_path_buf());
        Self { root: canonical }
    }
}

use application::ports::media_store::MediaStore;
use async_trait::async_trait;
use domain::media::ManagedBlobRef;

#[async_trait]
impl MediaStore for FsStore {
    async fn resolve_playable(
        &self,
        blob_ref: &ManagedBlobRef,
    ) -> Result<PlayableSource, DomainError> {
        let path = self.root.join(&blob_ref.relative_path);
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Err(DomainError::NotFound(format!(
                "File not found: {}",
                path.display()
            )));
        }

        Ok(PlayableSource::ResolvedPlayable {
            path: path.to_string_lossy().to_string(),
            duration_ms: None,
        })
    }
}
