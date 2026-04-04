use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;
use std::sync::Arc;

use adapters_voice::state_map::GuildStateMap;
use adapters_voice::track_event_handler::post_now_playing;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("clear")
        .description("Clear the current music queue")
        .default_member_permissions(Permissions::ADMINISTRATOR)
}

pub async fn run(
    http: &Arc<serenity::all::Http>,
    interaction: &CommandInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = match guild_state_map.get(&guild_id) {
        Some(state) => state.clone(),
        None => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("No active playback in this server."),
                )
                .await;
            return;
        }
    };

    let mut state = state_lock.lock().await;

    if state.meta_queue.is_empty() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is already empty."),
            )
            .await;
        return;
    }

    let cleared_count = state.meta_queue.len();
    state.meta_queue.clear();
    state.cancel_auto_leave();

    // Grab text_channel and msg_id before dropping the lock
    let text_channel = state.text_channel_id;
    let msg_id = state.now_playing_msg;
    drop(state);

    // Stop Songbird's builtin queue
    if let Some(handler_lock) = songbird.get(guild_id) {
        handler_lock.lock().await.queue().stop();
    }

    // Update the now-playing embed to show "Queue Ended"
    if let Some(channel_id) = text_channel {
        post_now_playing(http, channel_id, guild_id, &state_lock, None, msg_id).await;
    }

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new()
                .content(format!("Cleared {} tracks from the queue.", cleared_count)),
        )
        .await;
}
