use crate::ports::playback_gateway::PlaybackGateway;
use domain::error::DomainError;
use domain::playback::QueueRequest;
use std::sync::Arc;

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
