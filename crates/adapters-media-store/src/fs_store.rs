use domain::error::DomainError;
use domain::media::PlayableSource;
use std::path::PathBuf;

/// Resolves a blob_location (relative to MEDIA_ROOT) to a PlayableSource.
pub fn resolve_blob_to_playable(
    media_root: &std::path::Path,
    blob_location: &str,
) -> Result<PlayableSource, DomainError> {
    let path = media_root.join(blob_location);
    if !path.exists() {
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

pub struct FsStore {
    #[allow(dead_code)]
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
