use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;
use std::sync::Arc;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("shuffle")
        .description("Shuffle the queue (keeps current track)")
        .default_member_permissions(Permissions::SEND_MESSAGES)
}

pub async fn run(
    http: &Arc<serenity::all::Http>,
    interaction: &CommandInteraction,
    guild_state_map: &Arc<GuildStateMap>,
    songbird: &Arc<songbird::Songbird>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = if let Some(s) = guild_state_map.get(&guild_id) {
        s.clone()
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is empty."),
            )
            .await;
        return;
    };

    let mut state = state_lock.lock().await;

    if state.meta_queue.len() <= 2 {
        // Only current track + 0-1 others, nothing to shuffle
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Not enough tracks to shuffle."),
            )
            .await;
        return;
    }

    // Shuffle positions 1..end using Fisher-Yates
    let mut tail: Vec<_> = state.meta_queue.drain(1..).collect();
    fastrand::shuffle(&mut tail);
    for t in tail {
        state.meta_queue.push_back(t);
    }
    drop(state);

    // Reorder Songbird's queue to match
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            if q.len() > 1 {
                let mut tail: Vec<_> = q.drain(1..).collect();
                fastrand::shuffle(&mut tail);
                for t in tail {
                    q.push_back(t);
                }
            }
        });
    }

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new().content("🔀 Queue shuffled."),
        )
        .await;
}
