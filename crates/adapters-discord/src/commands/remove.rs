use serenity::builder::{CreateCommand, CreateCommandOption, EditInteractionResponse};
use serenity::model::application::{CommandInteraction, CommandOptionType};
use serenity::model::permissions::Permissions;
use std::sync::Arc;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("remove")
        .description("Remove a track from the queue")
        .default_member_permissions(Permissions::SEND_MESSAGES)
        .add_option(
            CreateCommandOption::new(CommandOptionType::String, "position", "Track to remove")
                .required(true)
                .set_autocomplete(true),
        )
}

pub async fn run(
    http: &Arc<serenity::all::Http>,
    interaction: &CommandInteraction,
    guild_state_map: &Arc<GuildStateMap>,
    songbird: &Arc<songbird::Songbird>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let guild_id = interaction.guild_id.unwrap_or_default();

    let position = interaction
        .data
        .options
        .first()
        .and_then(|opt| opt.value.as_str())
        .and_then(|s| s.parse::<usize>().ok());

    let Some(pos) = position else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a valid track position."),
            )
            .await;
        return;
    };

    if pos == 0 {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("Can't remove the currently playing track. Use /skip instead."),
            )
            .await;
        return;
    }

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

    if pos >= state.meta_queue.len() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content(format!("Position {pos} doesn't exist in the queue.")),
            )
            .await;
        return;
    }

    let removed = state.meta_queue.remove(pos).unwrap();

    // Remove from Songbird's queue while we still hold the state lock.
    // These are Queued (not Playing) entries at pos > 0, so dropping
    // them does NOT fire TrackEvent::End. We update meta_queue first
    // to keep it as the source of truth.
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            if pos < q.len() {
                q.remove(pos);
            }
        });
    }

    drop(state);

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new()
                .content(format!("🗑️ Removed **{}** from the queue.", removed.title)),
        )
        .await;
}
