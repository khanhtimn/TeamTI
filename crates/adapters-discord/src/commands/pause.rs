use std::sync::Arc;

use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("pause")
        .description("Pause the currently playing track")
        .default_member_permissions(Permissions::SEND_MESSAGES)
}

pub async fn run(
    http: &Arc<serenity::all::Http>,
    interaction: &CommandInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = if let Some(state) = guild_state_map.get(&guild_id) {
        state.clone()
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("The bot isn't in a voice channel. Use /play to start."),
            )
            .await;
        return;
    };

    let mut state = state_lock.lock().await;

    if state.meta_queue.is_empty() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Nothing is currently playing."),
            )
            .await;
        return;
    }

    if state.is_paused() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Already paused. Use /resume to continue."),
            )
            .await;
        return;
    }

    // Record pause timestamp
    state.paused_at = Some(std::time::Instant::now());
    drop(state);

    // Pause Songbird's queue
    if let Some(handler_lock) = songbird.get(guild_id) {
        let _ = handler_lock.lock().await.queue().pause();
    }

    let _ = interaction
        .edit_response(http, EditInteractionResponse::new().content("⏸ Paused."))
        .await;
}
