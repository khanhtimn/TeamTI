use async_trait::async_trait;
use domain::media::{ManagedBlobRef, PlayableSource, MediaOrigin};
use domain::error::DomainError;
use application::ports::media_store::MediaStore;
use std::path::{Path, PathBuf};

pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        std::fs::create_dir_all(root.as_ref()).ok();
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl MediaStore for FsStore {
    async fn resolve_playable(&self, blob_ref: &ManagedBlobRef) -> Result<PlayableSource, DomainError> {
        let path = PathBuf::from(&blob_ref.absolute_path);
        if !path.exists() {
            return Err(DomainError::NotFound(format!("File not found: {}", path.display())));
        }
        
        Ok(PlayableSource::ResolvedPlayable {
            path: blob_ref.absolute_path.clone(),
            duration_ms: None, // Will be parsed later
        })
    }

    async fn import_local(&self, source_path: &str) -> Result<(ManagedBlobRef, MediaOrigin), DomainError> {
        let src = PathBuf::from(source_path);
        if !src.exists() {
            return Err(DomainError::NotFound(format!("Source file not found: {}", source_path)));
        }

        let file_name = src.file_name()
            .ok_or_else(|| DomainError::InvalidState("Invalid file name".to_string()))?;
        
        let unique_name = format!("{}_{}", uuid::Uuid::new_v4(), file_name.to_string_lossy());
        let dest = self.root.join(&unique_name);
        
        std::fs::copy(&src, &dest).map_err(|e| DomainError::InvalidState(e.to_string()))?;

        let rel_path = unique_name;
        
        Ok((
            ManagedBlobRef {
                absolute_path: dest.to_string_lossy().to_string(),
            },
            MediaOrigin::LocalManaged { rel_path },
        ))
    }
}
