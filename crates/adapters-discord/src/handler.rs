use std::path::PathBuf;
use std::sync::Arc;

use serenity::async_trait;
use serenity::model::application::Command;
use serenity::model::event::FullEvent;
use serenity::prelude::{Context, EventHandler};

use adapters_persistence::repositories::track_repository::PgTrackRepository;

#[derive(Clone)]
pub struct DiscordEventHandler {
    pub discord_guild_id: u64,
    pub track_repo: Arc<PgTrackRepository>,
    pub media_root: PathBuf,
    pub auto_leave_secs: u64,
    pub songbird: Arc<songbird::Songbird>,
    pub guild_state: Arc<adapters_voice::state_map::GuildStateMap>,
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    async fn dispatch(&self, ctx: &Context, event: &FullEvent) {
        match event {
            FullEvent::Ready { data_about_bot, .. } => {
                use crate::commands::{clear, leave, play, rescan};

                tracing::info!(
                    username   = %data_about_bot.user.name,
                    guild_count = data_about_bot.guilds.len(),
                    operation  = "discord.ready",
                    "bot connected to Discord"
                );

                let cmds = vec![
                    play::register(),
                    clear::register(),
                    leave::register(),
                    rescan::register(),
                ];

                // Clear all old global commands to prevent duplication
                if let Err(e) = Command::set_global_commands(&ctx.http, &[]).await {
                    tracing::warn!("Failed to clear global commands: {}", e);
                }

                // Register commands specifically to the configured guild for instant propagation
                use serenity::all::GuildId;
                let guild_id = GuildId::new(self.discord_guild_id);
                let result = guild_id.set_commands(&ctx.http, &cmds).await;

                match result {
                    Ok(cmds) => tracing::info!(
                        guild_id = self.discord_guild_id,
                        count = cmds.len(),
                        operation = "discord.commands_registered",
                        "registered guild slash commands"
                    ),
                    Err(e) => tracing::error!(
                        error     = %e,
                        operation = "discord.commands_register_failed",
                        "failed to register slash commands"
                    ),
                }
            }

            FullEvent::InteractionCreate { interaction, .. } => {
                use serenity::model::application::Interaction;

                match interaction {
                    Interaction::Command(cmd) => {
                        let name = cmd.data.name.as_str();
                        tracing::debug!(
                            command    = name,
                            user_id    = %cmd.user.id,
                            guild_id   = %cmd.guild_id.unwrap_or_default(),
                            operation  = "discord.command",
                        );
                        match name {
                            "play" => {
                                crate::commands::play::run(
                                    &ctx.http,
                                    &ctx.cache,
                                    cmd,
                                    &self.track_repo,
                                    &self.media_root,
                                    self.auto_leave_secs,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await
                            }
                            "clear" => {
                                crate::commands::clear::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await
                            }
                            "leave" => {
                                crate::commands::leave::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await
                            }
                            "rescan" => crate::commands::rescan::run(&ctx.http, cmd).await,
                            unknown => tracing::warn!(
                                command = unknown,
                                operation = "discord.unknown_command",
                                "received unknown command"
                            ),
                        }
                    }

                    Interaction::Autocomplete(ac) if ac.data.name == "play" => {
                        crate::commands::play::autocomplete(&ctx.http, ac, &self.track_repo).await;
                    }

                    _ => {}
                }
            }

            _ => {}
        }
    }
}
