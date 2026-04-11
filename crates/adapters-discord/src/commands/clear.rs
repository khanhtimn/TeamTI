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

    state.meta_queue.truncate(1);

    // Maintain precisely the explicit subset of tracked items inside our state snapshot securely tracking only mapped handles natively
    let retained_uuids: Vec<_> = state
        .meta_queue
        .iter()
        .filter_map(|t| t.songbird_uuid)
        .collect();
    drop(state);

    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            let mut retained = Vec::new();
            for handle in q.drain(..) {
                if retained_uuids.contains(&handle.handle().uuid()) {
                    retained.push(handle);
                }
            }
            for handle in retained {
                q.push_back(handle);
            }
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
