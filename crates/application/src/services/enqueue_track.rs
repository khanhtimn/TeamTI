use std::sync::Arc;
use domain::error::DomainError;
use domain::playback::EnqueueRequest;
use crate::ports::playback_gateway::PlaybackGateway;

pub struct EnqueueTrack {
    gateway: Arc<dyn PlaybackGateway>,
}

impl EnqueueTrack {
    pub fn new(gateway: Arc<dyn PlaybackGateway>) -> Self {
        Self { gateway }
    }

    pub async fn execute(&self, req: EnqueueRequest) -> Result<(), DomainError> {
        self.gateway.enqueue(req).await
    }
}
