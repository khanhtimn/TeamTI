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

    // Shuffle positions 1..end using a shared index permutation.
    // meta_queue is the source of truth; Songbird is reordered to match.
    let tail: Vec<_> = state.meta_queue.drain(1..).collect();
    let original_len = tail.len();

    // Build a single shuffled index permutation
    let mut indices: Vec<usize> = (0..original_len).collect();
    fastrand::shuffle(&mut indices);

    // Rebuild meta_queue tail in the shuffled order
    // Use Options to move items without Clone issues
    let mut opts: Vec<_> = tail.into_iter().map(Some).collect();
    for &old_pos in &indices {
        if let Some(item) = opts[old_pos].take() {
            state.meta_queue.push_back(item);
        }
    }
    drop(state);

    // Apply the same permutation to Songbird's queue
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            if q.len() > 1 {
                let mut sb_tail: Vec<_> = q.drain(1..).collect();
                if sb_tail.len() == original_len {
                    let mut sb_opts: Vec<_> = sb_tail.drain(..).map(Some).collect();
                    for &old_pos in &indices {
                        if let Some(item) = sb_opts[old_pos].take() {
                            q.push_back(item);
                        }
                    }
                } else {
                    for item in sb_tail {
                        q.push_back(item);
                    }
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
