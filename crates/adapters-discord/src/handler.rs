use serenity::async_trait;
use serenity::builder::{
    CreateAutocompleteResponse, CreateInteractionResponse, CreateInteractionResponseMessage,
    AutocompleteChoice, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, Interaction};
use serenity::model::event::FullEvent;
use serenity::model::id::GuildId;
use serenity::model::Permissions;
use serenity::prelude::*;
use std::sync::Arc;
use tracing::{error, info, warn};

use application::ports::media_repository::MediaRepository;
use application::services::{
    enqueue_track::EnqueueTrack, join_voice::JoinVoice, leave_voice::LeaveVoice,
};
use adapters_media_store::scanner::MediaScanner;
use domain::guild::GuildId as DomainGuildId;
use domain::playback::{QueueRequest, StartVoiceChannel};

use crate::register;
use crate::response::{respond_error, respond_success};

pub struct DiscordHandler {
    pub target_guild_id: u64,
    pub join_voice: Arc<JoinVoice>,
    pub leave_voice: Arc<LeaveVoice>,
    pub enqueue_track: Arc<EnqueueTrack>,
    pub media_repo: Arc<dyn MediaRepository>,
    pub scanner: Arc<MediaScanner>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn dispatch(&self, ctx: &Context, event: &FullEvent) {
        match event {
            FullEvent::Ready { data_about_bot, .. } => {
                info!("{} is connected!", data_about_bot.user.name);
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
            FullEvent::InteractionCreate { interaction, .. } => {
                match interaction {
                    Interaction::Command(command) => {
                        self.handle_command(ctx, command).await;
                    }
                    Interaction::Autocomplete(autocomplete) => {
                        self.handle_autocomplete(ctx, autocomplete).await;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl DiscordHandler {
    async fn handle_command(&self, ctx: &Context, command: &CommandInteraction) {
        match command.data.name.as_str() {
            "ping" => {
                let _ = respond_success(ctx, command, "Pong!").await;
            }
            "join" => {
                self.handle_join(ctx, command).await;
            }
            "leave" => {
                self.handle_leave(ctx, command).await;
            }
            "play" => {
                self.handle_play(ctx, command).await;
            }
            "scan" => {
                self.handle_scan(ctx, command).await;
            }
            _ => {
                let _ = respond_error(ctx, command, "Unknown command").await;
            }
        }
    }

    async fn handle_join(&self, ctx: &Context, command: &CommandInteraction) {
        let guild_id = match command.guild_id {
            Some(id) => id,
            None => {
                let _ = respond_error(ctx, command, "Commands must be used in a guild.").await;
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
                    let _ =
                        respond_error(ctx, command, &format!("Failed to join: {:?}", e)).await;
                } else {
                    let _ = respond_success(ctx, command, "Joined voice!").await;
                }
            }
            None => {
                let _ = respond_error(ctx, command, "You must be in a voice channel first.").await;
            }
        }
    }

    async fn handle_leave(&self, ctx: &Context, command: &CommandInteraction) {
        if let Some(guild_id) = command.guild_id {
            if let Err(e) = self
                .leave_voice
                .execute(DomainGuildId(guild_id.get()))
                .await
            {
                let _ = respond_error(ctx, command, &format!("Failed to leave: {:?}", e)).await;
            } else {
                let _ = respond_success(ctx, command, "Left voice channel.").await;
            }
        }
    }

    async fn handle_play(&self, ctx: &Context, command: &CommandInteraction) {
        let query_val = command
            .data
            .options
            .iter()
            .find(|o| o.name == "query")
            .and_then(|o| o.value.as_str());

        let guild_id = match command.guild_id {
            Some(id) => id,
            None => {
                let _ = respond_error(ctx, command, "Must be used in a guild.").await;
                return;
            }
        };

        let asset_id_str = match query_val {
            Some(v) => v,
            None => {
                let _ = respond_error(ctx, command, "No track selected.").await;
                return;
            }
        };

        let asset_id = match uuid::Uuid::parse_str(asset_id_str) {
            Ok(id) => id,
            Err(_) => {
                let _ = respond_error(ctx, command, "Invalid track selection.").await;
                return;
            }
        };

        let gid = DomainGuildId(guild_id.get());
        let user_id = command.user.id.get();

        match self.enqueue_track.execute_by_asset_id(asset_id, gid, user_id).await {
            Ok(title) => {
                let _ = respond_success(ctx, command, &format!("Enqueued `{title}`")).await;
            }
            Err(e) => {
                let _ =
                    respond_error(ctx, command, &format!("Failed to enqueue: {:?}", e)).await;
            }
        }
    }

    async fn handle_scan(&self, ctx: &Context, command: &CommandInteraction) {
        let _guild_id = match command.guild_id {
            Some(id) => id,
            None => {
                let _ = respond_error(ctx, command, "Must be used in a guild.").await;
                return;
            }
        };

        // Check ADMINISTRATOR permission
        let has_admin = command
            .member
            .as_ref()
            .and_then(|m| m.permissions)
            .map(|p| p.contains(Permissions::ADMINISTRATOR))
            .unwrap_or(false);

        if !has_admin {
            let _ = respond_error(ctx, command, "You must be an administrator to use /scan.").await;
            return;
        }

        // Defer — scanning may take a while
        let defer = CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new(),
        );
        if let Err(e) = command.create_response(&ctx.http, defer).await {
            error!("Failed to defer /scan response: {:?}", e);
            return;
        }

        match self.scanner.scan().await {
            Ok(report) => {
                let _ = command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(format!("✅ {report}")))
                    .await;
            }
            Err(e) => {
                let _ = command
                    .edit_response(&ctx.http, EditInteractionResponse::new().content(format!("❌ Scan failed: {e}")))
                    .await;
            }
        }
    }

    async fn handle_autocomplete(&self, ctx: &Context, autocomplete: &CommandInteraction) {
        if autocomplete.data.name.as_str() != "play" {
            return;
        }

        let focused_value = autocomplete
            .data
            .options
            .iter()
            .find(|o| o.name == "query")
            .and_then(|o| o.value.as_str())
            .unwrap_or("");

        let results = match self.media_repo.search(focused_value, 25).await {
            Ok(r) => r,
            Err(e) => {
                warn!("Autocomplete search failed: {:?}", e);
                Vec::new()
            }
        };

        let choices: Vec<AutocompleteChoice<'_>> = results
            .iter()
            .map(|asset| {
                let display = if let Some(ref filename) = asset.original_filename {
                    format!("{} ({})", asset.title, filename)
                } else {
                    asset.title.clone()
                };
                // Truncate display name to 100 chars (Discord limit)
                let display = if display.len() > 100 {
                    format!("{}…", &display[..99])
                } else {
                    display
                };
                AutocompleteChoice::new(display, asset.id.to_string())
            })
            .collect();

        let response = CreateInteractionResponse::Autocomplete(
            CreateAutocompleteResponse::new().set_choices(choices),
        );

        if let Err(e) = autocomplete.create_response(&ctx.http, response).await {
            warn!("Failed to send autocomplete response: {:?}", e);
        }
    }
}
