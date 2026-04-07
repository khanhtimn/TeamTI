use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;
use std::sync::Arc;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("clear")
        .description("Clear the music queue (keeps the currently playing track)")
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
                EditInteractionResponse::new().content("No active playback in this server."),
            )
            .await;
        return;
    };

    let mut state = state_lock.lock().await;

    if state.meta_queue.len() <= 1 {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is already empty."),
            )
            .await;
        return;
    }

    // Keep position 0 (currently playing), clear positions 1..end
    let cleared_count = state.meta_queue.len() - 1;

    // Drain meta_queue ourselves — don't rely on Songbird event handlers
    // which fire asynchronously and could pop the wrong entries.
    state.meta_queue.truncate(1);
    drop(state);

    // Remove from Songbird's queue (positions 1..end).
    // These are Queued (not Playing) entries — dropping them does NOT
    // fire TrackEvent::End, but we drain meta_queue first to be safe.
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            q.drain(1..);
        });
    }

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new().content(format!(
                "🗑️ Queue cleared ({cleared_count} tracks removed)."
            )),
        )
        .await;
}
