use async_trait::async_trait;
use domain::error::DomainError;
use domain::guild::GuildId;
use domain::playback::{EnqueueRequest, QueueRequest};

#[async_trait]
pub trait PlaybackGateway: Send + Sync {
    async fn join_voice(&self, req: QueueRequest) -> Result<(), DomainError>;
    async fn leave_voice(&self, guild_id: GuildId) -> Result<(), DomainError>;
    async fn enqueue(&self, req: EnqueueRequest) -> Result<(), DomainError>;
}
