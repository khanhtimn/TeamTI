use serenity::builder::{CreateCommand, CreateCommandOption, EditInteractionResponse};
use serenity::model::application::{CommandInteraction, CommandOptionType};
use serenity::model::permissions::Permissions;
use std::sync::Arc;

use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("move")
        .description("Move a track to a different position")
        .default_member_permissions(Permissions::SEND_MESSAGES)
        .add_option(
            CreateCommandOption::new(CommandOptionType::String, "from", "Track to move")
                .required(true)
                .set_autocomplete(true),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::Integer, "to", "New position (1-based)")
                .required(true)
                .min_int_value(1),
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

    let from_pos = interaction
        .data
        .options
        .iter()
        .find(|opt| opt.name == "from")
        .and_then(|opt| opt.value.as_str())
        .and_then(|s| s.parse::<usize>().ok());

    let to_pos = interaction
        .data
        .options
        .iter()
        .find(|opt| opt.name == "to")
        .and_then(|opt| opt.value.as_i64())
        .map(|n| n as usize);

    let (Some(from), Some(to)) = (from_pos, to_pos) else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please provide valid from/to positions."),
            )
            .await;
        return;
    };

    if from == 0 || to == 0 {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("Can't move the currently playing track. Use /skip instead."),
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
    let queue_len = state.meta_queue.len();

    if from >= queue_len {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content(format!("Position {from} doesn't exist in the queue.")),
            )
            .await;
        return;
    }

    let to = to.min(queue_len - 1); // clamp to valid range

    let track = state.meta_queue.remove(from).unwrap();
    let title = track.title.clone();
    state.meta_queue.insert(to, track);
    drop(state);

    // Reorder Songbird's queue
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            if from < q.len() {
                let item = q.remove(from).unwrap();
                let insert_pos = to.min(q.len());
                q.insert(insert_pos, item);
            }
        });
    }

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new()
                .content(format!("✓ Moved **{title}** to position {to}.")),
        )
        .await;
}
