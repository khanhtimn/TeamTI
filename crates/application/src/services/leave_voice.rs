use std::sync::Arc;
use domain::error::DomainError;
use domain::guild::GuildId;
use crate::ports::playback_gateway::PlaybackGateway;

pub struct LeaveVoice {
    gateway: Arc<dyn PlaybackGateway>,
}

impl LeaveVoice {
    pub fn new(gateway: Arc<dyn PlaybackGateway>) -> Self {
        Self { gateway }
    }

    pub async fn execute(&self, guild_id: GuildId) -> Result<(), DomainError> {
        self.gateway.leave_voice(guild_id).await
    }
}
