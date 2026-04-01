use std::sync::Arc;
use domain::error::DomainError;
use domain::playback::QueueRequest;
use crate::ports::playback_gateway::PlaybackGateway;

pub struct JoinVoice {
    gateway: Arc<dyn PlaybackGateway>,
}

impl JoinVoice {
    pub fn new(gateway: Arc<dyn PlaybackGateway>) -> Self {
        Self { gateway }
    }

    pub async fn execute(&self, req: QueueRequest) -> Result<(), DomainError> {
        self.gateway.join_voice(req).await
    }
}
