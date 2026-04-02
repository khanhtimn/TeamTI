use application::ports::playback_gateway::PlaybackGateway;
use async_trait::async_trait;
use domain::error::DomainError;
use domain::guild::GuildId;
use domain::playback::{EnqueueRequest, QueueRequest, StartVoiceChannel};
use serenity::model::id::ChannelId as SerenityChannelId;
use serenity::model::id::GuildId as SerenityGuildId;
use songbird::Songbird;
use songbird::events::{CoreEvent, TrackEvent};
use std::sync::Arc;
use tracing::{debug, error, info};

use crate::event_handler::SongbirdEventLogger;

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

        info!(%guild_id, %channel_id, "Joining voice channel");

        let call = self
            .songbird
            .join(guild_id, channel_id)
            .await
            .map_err(|e| DomainError::InvalidState(format!("Failed to join voice: {:?}", e)))?;

        // Register global event handlers for driver + track lifecycle diagnostics
        {
            let mut handler = call.lock().await;

            // Driver connection events
            handler.add_global_event(CoreEvent::DriverConnect.into(), SongbirdEventLogger);
            handler.add_global_event(CoreEvent::DriverDisconnect.into(), SongbirdEventLogger);
            handler.add_global_event(CoreEvent::DriverReconnect.into(), SongbirdEventLogger);

            // Track lifecycle events (global = fires for all tracks)
            handler.add_global_event(TrackEvent::Play.into(), SongbirdEventLogger);
            handler.add_global_event(TrackEvent::End.into(), SongbirdEventLogger);
            handler.add_global_event(TrackEvent::Error.into(), SongbirdEventLogger);
            handler.add_global_event(TrackEvent::Playable.into(), SongbirdEventLogger);

            info!(%guild_id, "Registered voice event handlers");
        }

        info!(%guild_id, "Successfully joined voice channel");
        Ok(())
    }

    async fn leave_voice(&self, guild_id: GuildId) -> Result<(), DomainError> {
        let g_id = SerenityGuildId::new(guild_id.0);
        self.songbird
            .leave(g_id)
            .await
            .map_err(|e| DomainError::InvalidState(format!("Failed to leave voice: {:?}", e)))?;
        info!(?guild_id, "Left voice channel");
        Ok(())
    }

    async fn enqueue(&self, req: EnqueueRequest) -> Result<(), DomainError> {
        let guild_id = SerenityGuildId::new(req.guild_id.0);

        if let Some(call) = self.songbird.get(guild_id) {
            let mut handler = call.lock().await;

            debug!(?req.source, "Mapping domain source to songbird input");

            // Map domain source to songbird source
            let source = crate::mapper::map_playable_to_songbird(req.source).await?;

            // Just using built in queue for v1
            handler.enqueue(source.into()).await;

            let queue_len = handler.queue().len();
            info!(%guild_id, queue_len, "Track enqueued successfully");

            Ok(())
        } else {
            error!(%guild_id, "Attempted to enqueue but bot is not in a voice channel");
            Err(DomainError::InvalidState(
                "Bot is not in voice channel".into(),
            ))
        }
    }
}
