use application::ports::playback_gateway::PlaybackGateway;
use async_trait::async_trait;
use domain::error::DomainError;
use domain::guild::GuildId;
use domain::playback::{EnqueueRequest, QueueRequest, StartVoiceChannel};
use serenity::model::id::ChannelId as SerenityChannelId;
use serenity::model::id::GuildId as SerenityGuildId;
use songbird::Songbird;
use std::sync::Arc;

pub struct SongbirdPlaybackGateway {
    songbird: Arc<Songbird>,
}

impl SongbirdPlaybackGateway {
    pub fn new(songbird: Arc<Songbird>) -> Self {
        Self { songbird }
    }
}

#[async_trait]
impl PlaybackGateway for SongbirdPlaybackGateway {
    async fn join_voice(&self, req: QueueRequest) -> Result<(), DomainError> {
        let StartVoiceChannel::Id(channel_id) = req.voice_channel_id;

        let guild_id = SerenityGuildId::new(req.guild_id.0);
        let channel_id = SerenityChannelId::new(channel_id);

        let _call = self
            .songbird
            .join(guild_id, channel_id)
            .await
            .map_err(|e| DomainError::InvalidState(format!("Failed to join voice: {:?}", e)))?;

        Ok(())
    }

    async fn leave_voice(&self, guild_id: GuildId) -> Result<(), DomainError> {
        let g_id = SerenityGuildId::new(guild_id.0);
        self.songbird
            .leave(g_id)
            .await
            .map_err(|e| DomainError::InvalidState(format!("Failed to leave voice: {:?}", e)))?;
        Ok(())
    }

    async fn enqueue(&self, req: EnqueueRequest) -> Result<(), DomainError> {
        let guild_id = SerenityGuildId::new(req.guild_id.0);

        if let Some(call) = self.songbird.get(guild_id) {
            let mut handler = call.lock().await;

            // Map domain source to songbird source
            let source = crate::mapper::map_playable_to_songbird(req.source).await?;

            // Just using built in queue for v1
            handler.enqueue(source.into()).await;

            Ok(())
        } else {
            Err(DomainError::InvalidState(
                "Bot is not in voice channel".into(),
            ))
        }
    }
}
