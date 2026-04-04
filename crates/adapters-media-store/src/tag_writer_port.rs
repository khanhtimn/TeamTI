use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use application::AppError;
use application::ports::file_ops::{FileTagWriterPort, TagData, WriteResult};

use crate::tag_writer::write_tags_atomic;

/// A1 fix: The adapter owns SMB semaphore acquisition and spawn_blocking.
/// The TagWriterWorker never acquires the semaphore — it delegates entirely
/// to this port, preventing double-permit acquisition.
pub struct FileTagWriterAdapter {
    pub media_root: PathBuf,
    pub smb_semaphore: Arc<Semaphore>,
}

#[async_trait]
impl FileTagWriterPort for FileTagWriterAdapter {
    async fn write_tags(
        &self,
        blob_location: &str,
        tags: &TagData,
    ) -> Result<WriteResult, AppError> {
        let abs_path = self.media_root.join(blob_location);
        let tags_data = tags.clone();

        let permit: OwnedSemaphorePermit = self
            .smb_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Io {
                path: Some(abs_path.clone()),
                source: std::io::Error::other("SMB semaphore closed"),
            })?;

        tokio::task::spawn_blocking(move || {
            let _permit = permit; // drops when closure returns
            write_tags_atomic(&abs_path, &tags_data)
        })
        .await
        .map_err(|e| AppError::Io {
            path: None,
            source: std::io::Error::other(format!("spawn_blocking panic: {e}")),
        })?
    }
}
