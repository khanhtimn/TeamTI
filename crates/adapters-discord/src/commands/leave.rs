use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;
use std::sync::Arc;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("leave")
        .description("Disconnect the bot from the voice channel")
        .default_member_permissions(Permissions::ADMINISTRATOR)
}

pub async fn run(
    http: &serenity::all::Http,
    interaction: &CommandInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let guild_id = interaction.guild_id.unwrap_or_default();

    // Check that we're actually connected before trying to leave
    if let Some(state_lock) = guild_state_map.get(&guild_id) {
        let mut state = state_lock.lock().await;

        if state.voice_channel_id.is_none() {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("I'm not currently in a voice channel."),
                )
                .await;
            return;
        }

        // Cancel any pending auto-leave timer
        state.cancel_auto_leave();

        // Clear our metadata mirror — Songbird queue is cleared inside leave_channel()
        state.meta_queue.clear();
        state.voice_channel_id = None;
        state.now_playing_msg = None;
        drop(state);
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("I'm not currently in a voice channel."),
            )
            .await;
        return;
    }

    // leave_channel() calls queue().stop() then songbird.leave()
    match adapters_voice::player::leave_channel(songbird, guild_id).await {
        Ok(_) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content("Left the voice channel and cleared the queue."),
                )
                .await;
        }
        Err(e) => {
            tracing::error!(
                guild_id  = %guild_id,
                error     = %e,
                operation = "leave.run",
                "failed to leave voice channel"
            );
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Failed to leave the voice channel."),
                )
                .await;
        }
    }
}
