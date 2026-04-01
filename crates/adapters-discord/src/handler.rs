use serenity::async_trait;
use serenity::model::application::Interaction;
use serenity::model::gateway::Ready;
use serenity::model::id::GuildId;
use serenity::prelude::*;
use std::sync::Arc;
use tracing::{error, info};

use application::services::{
    enqueue_track::EnqueueTrack, join_voice::JoinVoice, leave_voice::LeaveVoice,
    register_media::RegisterMedia,
};
use domain::guild::GuildId as DomainGuildId;
use domain::playback::{EnqueueRequest, QueueRequest, StartVoiceChannel};

use crate::register;
use crate::response::{respond_error, respond_success};

pub struct DiscordHandler {
    pub target_guild_id: u64,
    pub join_voice: Arc<JoinVoice>,
    pub leave_voice: Arc<LeaveVoice>,
    pub register_media: Arc<RegisterMedia>,
    pub enqueue_track: Arc<EnqueueTrack>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);
        let guild_id = GuildId::new(self.target_guild_id);

        if let Err(e) = register::register_guild_commands(&ctx.http, guild_id).await {
            error!("Failed to register commands: {:?}", e);
        } else {
            info!(
                "Registered guild commands for guild {}",
                self.target_guild_id
            );
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            match command.data.name.as_str() {
                "ping" => {
                    let _ = respond_success(&ctx, &command, "Pong!").await;
                }
                "join" => {
                    let guild_id = match command.guild_id {
                        Some(id) => id,
                        None => {
                            let _ =
                                respond_error(&ctx, &command, "Commands must be used in a guild.")
                                    .await;
                            return;
                        }
                    };

                    let channel_id = ctx.cache.guild(guild_id).and_then(|guild| {
                        guild
                            .voice_states
                            .get(&command.user.id)
                            .and_then(|vs| vs.channel_id)
                    });

                    match channel_id {
                        Some(c_id) => {
                            let req = QueueRequest {
                                guild_id: DomainGuildId(guild_id.get()),
                                voice_channel_id: StartVoiceChannel::Id(c_id.get()),
                            };
                            if let Err(e) = self.join_voice.execute(req).await {
                                let _ = respond_error(
                                    &ctx,
                                    &command,
                                    &format!("Failed to join: {:?}", e),
                                )
                                .await;
                            } else {
                                let _ = respond_success(&ctx, &command, "Joined voice!").await;
                            }
                        }
                        None => {
                            let _ = respond_error(
                                &ctx,
                                &command,
                                "You must be in a voice channel first.",
                            )
                            .await;
                        }
                    }
                }
                "leave" => {
                    if let Some(guild_id) = command.guild_id {
                        if let Err(e) = self
                            .leave_voice
                            .execute(DomainGuildId(guild_id.get()))
                            .await
                        {
                            let _ =
                                respond_error(&ctx, &command, &format!("Failed to leave: {:?}", e))
                                    .await;
                        } else {
                            let _ = respond_success(&ctx, &command, "Left voice channel.").await;
                        }
                    }
                }
                "play_local" => {
                    let path_val = command
                        .data
                        .options
                        .iter()
                        .find(|o| o.name == "path")
                        .and_then(|o| o.value.as_str());
                    let guild_id = command.guild_id;

                    if path_val.is_none() || guild_id.is_none() {
                        let _ = respond_error(&ctx, &command, "Invalid path or guild").await;
                        return;
                    }

                    let path = path_val.unwrap().to_string();
                    let gid = DomainGuildId(guild_id.unwrap().get());
                    let user_id = command.user.id.get();

                    let reg_res = match self.register_media.execute_local(&path).await {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = respond_error(
                                &ctx,
                                &command,
                                &format!("Failed to register media: {:?}", e),
                            )
                            .await;
                            return;
                        }
                    };

                    // Assume we use MediaStore to get a resolved playable later, but for v1 it's local.
                    let enqueue_req = EnqueueRequest {
                        guild_id: gid,
                        user_id,
                        source: domain::media::PlayableSource::ResolvedPlayable {
                            path: path.clone(), // Should ideally use the resolved location from store, using input path for now
                            duration_ms: None,
                        },
                        asset_id: reg_res.asset_id,
                    };

                    if let Err(e) = self.enqueue_track.execute(enqueue_req).await {
                        let _ = respond_error(
                            &ctx,
                            &command,
                            &format!("Failed to enqueue track: {:?}", e),
                        )
                        .await;
                    } else {
                        let _ = respond_success(
                            &ctx,
                            &command,
                            &format!("Enqueued `{}`", reg_res.title),
                        )
                        .await;
                    }
                }
                _ => {
                    let _ = respond_error(&ctx, &command, "Unknown command").await;
                }
            }
        }
    }
}
