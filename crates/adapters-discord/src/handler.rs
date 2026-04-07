use std::path::PathBuf;
use std::sync::Arc;

use serenity::async_trait;
use serenity::model::application::Command;
use serenity::model::event::FullEvent;
use serenity::prelude::{Context, EventHandler};

use adapters_persistence::repositories::track_repository::PgTrackRepository;
use adapters_voice::lifecycle::TrackLifecycleTx;
use application::ports::playlist::PlaylistPort;
use application::ports::recommendation::RecommendationPort;
use application::ports::search::TrackSearchPort;
use application::ports::user_library::UserLibraryPort;

#[derive(Clone)]
pub struct DiscordEventHandler {
    pub discord_guild_id: u64,
    pub track_repo: Arc<PgTrackRepository>,
    pub search_port: Arc<dyn TrackSearchPort>,
    pub playlist_port: Arc<dyn PlaylistPort>,
    pub user_library_port: Arc<dyn UserLibraryPort>,
    pub recommendation_port: Arc<dyn RecommendationPort>,
    pub media_root: PathBuf,
    pub auto_leave_secs: u64,
    pub songbird: Arc<songbird::Songbird>,
    pub guild_state: Arc<adapters_voice::state_map::GuildStateMap>,
    pub lifecycle_tx: TrackLifecycleTx,
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    async fn dispatch(&self, ctx: &Context, event: &FullEvent) {
        match event {
            FullEvent::Ready { data_about_bot, .. } => {
                use crate::commands::{
                    clear, favourite, history, leave, move_track, nowplaying, pause, play,
                    playlist, queue, radio, remove, rescan, resume, shuffle, skip,
                };

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
                    playlist::register(),
                    favourite::register(),
                    radio::register(),
                    history::register(),
                    pause::register(),
                    resume::register(),
                    skip::register(),
                    nowplaying::register(),
                    queue::register(),
                    remove::register(),
                    move_track::register(),
                    shuffle::register(),
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
                                    &self.lifecycle_tx,
                                    &self.search_port,
                                )
                                .await;
                            }
                            "clear" => {
                                crate::commands::clear::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await;
                            }
                            "leave" => {
                                crate::commands::leave::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await;
                            }
                            "rescan" => {
                                crate::commands::rescan::run(&ctx.http, cmd, &self.search_port)
                                    .await;
                            }
                            "playlist" => {
                                let subcmd = cmd.data.options.first().map(|o| o.name.as_str());
                                if subcmd == Some("play") {
                                    crate::commands::playlist::run_play(
                                        &ctx.http,
                                        &ctx.cache,
                                        cmd,
                                        &self.playlist_port,
                                        &self.media_root,
                                        self.auto_leave_secs,
                                        &self.songbird,
                                        &self.guild_state,
                                        &self.lifecycle_tx,
                                    )
                                    .await;
                                } else {
                                    crate::commands::playlist::run(
                                        &ctx.http,
                                        cmd,
                                        &self.playlist_port,
                                    )
                                    .await;
                                }
                            }
                            "favourite" => {
                                crate::commands::favourite::run(
                                    &ctx.http,
                                    cmd,
                                    &self.user_library_port,
                                    &self.guild_state,
                                )
                                .await;
                            }
                            "radio" => {
                                crate::commands::radio::run(
                                    &ctx.http,
                                    cmd,
                                    &self.guild_state,
                                    &self.lifecycle_tx,
                                )
                                .await;
                            }
                            "history" => {
                                crate::commands::history::run(
                                    &ctx.http,
                                    cmd,
                                    &self.user_library_port,
                                )
                                .await;
                            }
                            // ── Pass 5: new commands ────────────────────
                            "pause" => {
                                crate::commands::pause::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await;
                            }
                            "resume" => {
                                crate::commands::resume::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                )
                                .await;
                            }
                            "skip" => {
                                crate::commands::skip::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                    &self.lifecycle_tx,
                                )
                                .await;
                            }
                            "nowplaying" => {
                                crate::commands::nowplaying::run(&ctx.http, cmd, &self.guild_state)
                                    .await;
                            }
                            "queue" => {
                                crate::commands::queue::run(
                                    &ctx.http,
                                    cmd,
                                    &self.songbird,
                                    &self.guild_state,
                                    &self.playlist_port,
                                )
                                .await;
                            }
                            "remove" => {
                                crate::commands::remove::run(
                                    &ctx.http,
                                    cmd,
                                    &self.guild_state,
                                    &self.songbird,
                                )
                                .await;
                            }
                            "move" => {
                                crate::commands::move_track::run(
                                    &ctx.http,
                                    cmd,
                                    &self.guild_state,
                                    &self.songbird,
                                )
                                .await;
                            }
                            "shuffle" => {
                                crate::commands::shuffle::run(
                                    &ctx.http,
                                    cmd,
                                    &self.guild_state,
                                    &self.songbird,
                                )
                                .await;
                            }
                            unknown => tracing::warn!(
                                command = unknown,
                                operation = "discord.unknown_command",
                                "received unknown command"
                            ),
                        }
                    }

                    Interaction::Autocomplete(ac) => match ac.data.name.as_str() {
                        "play" => {
                            crate::commands::play::autocomplete(
                                &ctx.http,
                                ac,
                                &self.search_port,
                                &self.user_library_port,
                                &self.recommendation_port,
                            )
                            .await;
                        }
                        "playlist" => {
                            crate::commands::playlist::autocomplete(
                                &ctx.http,
                                ac,
                                &self.playlist_port,
                                &self.search_port,
                            )
                            .await;
                        }
                        "favourite" => {
                            crate::commands::favourite::autocomplete(
                                &ctx.http,
                                ac,
                                &self.search_port,
                            )
                            .await;
                        }
                        "skip" => {
                            crate::commands::skip::autocomplete(&ctx.http, ac, &self.guild_state)
                                .await;
                        }
                        "remove" | "move" => {
                            crate::commands::queue::autocomplete(&ctx.http, ac, &self.guild_state)
                                .await;
                        }
                        _ => {}
                    },

                    Interaction::Component(component) => {
                        self.handle_component_interaction(ctx, component).await;
                    }

                    _ => {}
                }
            }

            _ => {}
        }
    }
}

impl DiscordEventHandler {
    async fn handle_component_interaction(
        &self,
        ctx: &Context,
        interaction: &serenity::model::application::ComponentInteraction,
    ) {
        use crate::commands::pagination;

        let custom_id = &interaction.data.custom_id;

        // Skip page indicator buttons (they're disabled and shouldn't fire)
        if custom_id.contains("page_indicator") {
            return;
        }

        if crate::ui::custom_id::QueueAction::is_queue_action(custom_id) {
            crate::commands::queue::handle_queue_button(
                &ctx.http,
                &ctx.cache,
                interaction,
                &self.songbird,
                &self.guild_state,
            )
            .await;
            return;
        }

        if crate::ui::custom_id::NPAction::is_np_action(custom_id) {
            crate::commands::nowplaying::handle_np_button(
                &ctx.http,
                interaction,
                &self.songbird,
                &self.guild_state,
            )
            .await;
            return;
        }

        let Some((view_type, resource_id, page, session_user_id)) =
            pagination::parse_custom_id(custom_id)
        else {
            return;
        };

        // Check session ownership
        if !pagination::is_session_owner(interaction, &session_user_id) {
            pagination::send_not_yours(&ctx.http, interaction).await;
            return;
        }

        match view_type.as_str() {
            "playlist_page" => {
                let Some(playlist_id) = pagination::parse_resource_uuid(&resource_id) else {
                    return;
                };

                match self
                    .playlist_port
                    .get_playlist_items(playlist_id, &session_user_id, page, pagination::PAGE_SIZE)
                    .await
                {
                    Ok(page_data) => {
                        let pages = pagination::total_pages(page_data.total, pagination::PAGE_SIZE);
                        let embed = crate::commands::playlist::build_playlist_embed(
                            &page_data.items,
                            playlist_id,
                            page,
                            pages,
                            page_data.total,
                        );
                        let buttons = pagination::build_nav_buttons(
                            "playlist_page",
                            &resource_id,
                            page,
                            pages,
                            &session_user_id,
                        );

                        let resp = serenity::builder::CreateInteractionResponse::UpdateMessage(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .embed(embed)
                                .components(vec![buttons]),
                        );
                        let _ = interaction.create_response(&ctx.http, resp).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pagination: failed to fetch playlist page");
                    }
                }
            }

            "fav_page" => {
                match self
                    .user_library_port
                    .list_favourites(&session_user_id, page, pagination::PAGE_SIZE)
                    .await
                {
                    Ok(page_data) => {
                        let pages = pagination::total_pages(page_data.total, pagination::PAGE_SIZE);
                        let embed = crate::commands::favourite::build_favourites_embed(
                            &page_data.tracks,
                            page,
                            pages,
                            page_data.total,
                        );
                        let buttons = pagination::build_nav_buttons(
                            "fav_page",
                            &resource_id,
                            page,
                            pages,
                            &session_user_id,
                        );

                        let resp = serenity::builder::CreateInteractionResponse::UpdateMessage(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .embed(embed)
                                .components(vec![buttons]),
                        );
                        let _ = interaction.create_response(&ctx.http, resp).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pagination: failed to fetch favourites page");
                    }
                }
            }

            "history_page" => {
                match self
                    .user_library_port
                    .recent_history(&session_user_id, 50)
                    .await
                {
                    Ok(all_tracks) => {
                        let total = all_tracks.len() as i64;
                        let pages = pagination::total_pages(total, pagination::PAGE_SIZE);
                        let start = (page * pagination::PAGE_SIZE) as usize;
                        let end = ((page + 1) * pagination::PAGE_SIZE) as usize;
                        let page_tracks: Vec<_> = all_tracks
                            .into_iter()
                            .skip(start)
                            .take(end - start)
                            .collect();

                        let embed = crate::commands::history::build_history_embed(
                            &page_tracks,
                            page,
                            pages,
                            total,
                        );
                        let buttons = pagination::build_nav_buttons(
                            "history_page",
                            &resource_id,
                            page,
                            pages,
                            &session_user_id,
                        );

                        let resp = serenity::builder::CreateInteractionResponse::UpdateMessage(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .embed(embed)
                                .components(vec![buttons]),
                        );
                        let _ = interaction.create_response(&ctx.http, resp).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pagination: failed to fetch history page");
                    }
                }
            }

            _ => {}
        }
    }
}
