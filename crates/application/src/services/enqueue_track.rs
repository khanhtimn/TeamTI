use crate::ports::media_store::MediaStore;
use crate::ports::repository::TrackRepository;
use domain::error::DomainError;
use domain::guild::GuildId;
use domain::media::ManagedBlobRef;
use domain::playback::EnqueueRequest;
use std::sync::Arc;

pub struct EnqueueTrackResult {
    pub title: String,
    pub artist: Option<String>,
}

pub struct EnqueueTrack {
    gateway: Arc<dyn crate::ports::playback_gateway::PlaybackGateway>,
    track_repo: Arc<dyn TrackRepository>,
    media_store: Arc<dyn MediaStore>,
}

impl EnqueueTrack {
    pub fn new(
        gateway: Arc<dyn crate::ports::playback_gateway::PlaybackGateway>,
        track_repo: Arc<dyn TrackRepository>,
        media_store: Arc<dyn MediaStore>,
    ) -> Self {
        Self {
            gateway,
            track_repo,
            media_store,
        }
    }

    /// Look up a track by ID, resolve its blob path, and enqueue for playback.
    pub async fn execute_by_asset_id(
        &self,
        asset_id: uuid::Uuid,
        guild_id: GuildId,
        user_id: u64,
    ) -> Result<EnqueueTrackResult, DomainError> {
        let track = self
            .track_repo
            .find_by_id(asset_id)
            .await
            .map_err(|e| DomainError::InvalidState(e.to_string()))?
            .ok_or_else(|| DomainError::NotFound(format!("No track with id {asset_id}")))?;

        let blob_ref = ManagedBlobRef {
            relative_path: track.blob_location.clone(),
        };
        let source = self.media_store.resolve_playable(&blob_ref).await?;

        let req = EnqueueRequest {
            guild_id,
            user_id,
            source,
            asset_id: track.id,
        };

        self.gateway.enqueue(req).await?;
        Ok(EnqueueTrackResult {
            title: track.title,
            artist: track.artist_display,
        })
    }

    pub async fn execute(&self, req: EnqueueRequest) -> Result<(), DomainError> {
        self.gateway.enqueue(req).await
    }
}
