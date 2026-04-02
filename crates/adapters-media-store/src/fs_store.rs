use application::ports::media_store::MediaStore;
use async_trait::async_trait;
use domain::error::DomainError;
use domain::media::{ManagedBlobRef, PlayableSource};
use std::path::{Path, PathBuf};

pub struct FsStore {
    #[allow(dead_code)]
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        std::fs::create_dir_all(root.as_ref()).ok();
        let canonical =
            std::fs::canonicalize(root.as_ref()).unwrap_or_else(|_| root.as_ref().to_path_buf());
        Self { root: canonical }
    }
}

#[async_trait]
impl MediaStore for FsStore {
    async fn resolve_playable(
        &self,
        blob_ref: &ManagedBlobRef,
    ) -> Result<PlayableSource, DomainError> {
        let path = PathBuf::from(&blob_ref.absolute_path);
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Err(DomainError::NotFound(format!(
                "File not found: {}",
                path.display()
            )));
        }

        Ok(PlayableSource::ResolvedPlayable {
            path: blob_ref.absolute_path.clone(),
            duration_ms: None,
        })
    }
}
