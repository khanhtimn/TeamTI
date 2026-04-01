use std::sync::Arc;
use domain::error::DomainError;
use domain::media::{ManagedBlobRef, MediaOrigin};
use domain::playback::EnqueueRequest;
use domain::guild::GuildId;
use crate::ports::playback_gateway::PlaybackGateway;
use crate::ports::media_repository::MediaRepository;
use crate::ports::media_store::MediaStore;

pub struct EnqueueTrack {
    gateway: Arc<dyn PlaybackGateway>,
    media_repo: Arc<dyn MediaRepository>,
    media_store: Arc<dyn MediaStore>,
}

impl EnqueueTrack {
    pub fn new(
        gateway: Arc<dyn PlaybackGateway>,
        media_repo: Arc<dyn MediaRepository>,
        media_store: Arc<dyn MediaStore>,
    ) -> Self {
        Self { gateway, media_repo, media_store }
    }

    /// Look up an asset by ID, resolve its blob path, and enqueue for playback.
    pub async fn execute_by_asset_id(
        &self,
        asset_id: uuid::Uuid,
        guild_id: GuildId,
        user_id: u64,
    ) -> Result<String, DomainError> {
        let asset = self.media_repo.find_by_id(asset_id).await?.ok_or_else(|| {
            DomainError::NotFound(format!("No asset with id {asset_id}"))
        })?;

        let blob_path = match &asset.origin {
            MediaOrigin::LocalManaged { rel_path } => rel_path.clone(),
            MediaOrigin::Remote(url) => {
                return Err(DomainError::InvalidState(
                    format!("Remote playback not yet supported: {url}"),
                ));
            }
        };

        let blob_ref = ManagedBlobRef { absolute_path: blob_path };
        let source = self.media_store.resolve_playable(&blob_ref).await?;

        let req = EnqueueRequest {
            guild_id,
            user_id,
            source,
            asset_id: asset.id,
        };

        self.gateway.enqueue(req).await?;
        Ok(asset.title)
    }

    pub async fn execute(&self, req: EnqueueRequest) -> Result<(), DomainError> {
        self.gateway.enqueue(req).await
    }
}
