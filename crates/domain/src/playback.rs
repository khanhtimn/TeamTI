use uuid::Uuid;
use crate::guild::GuildId;
use crate::media::PlayableSource;

#[derive(Debug, Clone)]
pub struct EnqueueRequest {
    pub guild_id: GuildId,
    pub user_id: u64,
    pub source: PlayableSource,
    pub asset_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct QueueRequest {
    pub guild_id: GuildId,
    pub voice_channel_id: StartVoiceChannel,
}

#[derive(Debug, Clone)]
pub enum StartVoiceChannel {
    Id(u64),
}
