use std::sync::Arc;

use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("resume")
        .description("Resume playback after pausing")
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
                EditInteractionResponse::new().content("Queue is empty."),
            )
            .await;
        return;
    }

    if !state.is_paused() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Playback isn't paused."),
            )
            .await;
        return;
    }

    // Accumulate paused time
    if let Some(pa) = state.paused_at.take() {
        let paused_ms = i64::try_from(pa.elapsed().as_millis()).unwrap_or_default();
        state.total_paused_ms += paused_ms;
    }
    drop(state);

    // Resume Songbird's queue
    if let Some(handler_lock) = songbird.get(guild_id) {
        let _ = handler_lock.lock().await.queue().resume();
    }

    let _ = interaction
        .edit_response(http, EditInteractionResponse::new().content("▶ Resumed."))
        .await;
}
