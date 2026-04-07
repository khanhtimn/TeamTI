use std::sync::Arc;

use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::id::ChannelId;
use serenity::model::permissions::Permissions;
use tokio_util::sync::CancellationToken;

use adapters_voice::state_map::GuildStateMap;
use adapters_voice::track_event_handler::{np_auto_update_task, post_now_playing_new};

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("nowplaying")
        .description("Show the currently playing track")
        .default_member_permissions(Permissions::SEND_MESSAGES)
}

pub async fn run(
    http: &Arc<serenity::all::Http>,
    interaction: &CommandInteraction,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let guild_id = interaction.guild_id.unwrap_or_default();
    let channel_id = ChannelId::new(interaction.channel_id.get());

    let state_lock = if let Some(state) = guild_state_map.get(&guild_id) {
        state.clone()
    } else {
        let _ = interaction.defer_ephemeral(http).await;
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Nothing is currently playing."),
            )
            .await;
        return;
    };

    {
        let state = state_lock.lock().await;
        if state.meta_queue.is_empty() {
            drop(state);
            let _ = interaction.defer_ephemeral(http).await;
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Nothing is currently playing."),
                )
                .await;
            return;
        }
    }

    // Defer as a public response (NP is public)
    let _ = interaction.defer(http).await;

    // Cancel old NP update task, post new NP embed, start new update task
    let track = {
        let state = state_lock.lock().await;
        state.meta_queue.front().cloned()
    };

    let new_msg_id =
        post_now_playing_new(http, channel_id, guild_id, &state_lock, track.as_ref()).await;

    if let Some(msg_id) = new_msg_id {
        let np_cancel = CancellationToken::new();
        {
            let mut state = state_lock.lock().await;
            state.cancel_np_update();
            state.np_update_cancel = Some(np_cancel.clone());
        }
        let http_clone = Arc::clone(http);
        let sl = state_lock.clone();
        tokio::spawn(np_auto_update_task(
            np_cancel, http_clone, channel_id, guild_id, msg_id, sl,
        ));
    }
}

pub async fn handle_np_button(
    http: &Arc<serenity::all::Http>,
    interaction: &serenity::model::application::ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let Some(action) = crate::ui::custom_id::NPAction::from_custom_id(&interaction.data.custom_id)
    else {
        return;
    };
    let guild_id = interaction.guild_id.unwrap_or_default();

    match action {
        crate::ui::custom_id::NPAction::Pause { .. } => {
            let state_lock = match guild_state_map.get(&guild_id) {
                Some(s) => s.clone(),
                None => return,
            };
            let mut state = state_lock.lock().await;

            if state.is_paused() {
                // Resume
                if let Some(pa) = state.paused_at.take() {
                    let paused_ms = pa.elapsed().as_millis() as i64;
                    state.total_paused_ms += paused_ms;
                }
                drop(state);
                if let Some(handler_lock) = songbird.get(guild_id) {
                    let _ = handler_lock.lock().await.queue().resume();
                }
            } else {
                // Pause
                state.paused_at = Some(std::time::Instant::now());
                drop(state);
                if let Some(handler_lock) = songbird.get(guild_id) {
                    let _ = handler_lock.lock().await.queue().pause();
                }
            }

            // Defer update so the background task overwrites it naturally in <= 1s
            let _ = interaction
                .create_response(
                    http,
                    serenity::builder::CreateInteractionResponse::Acknowledge,
                )
                .await;
        }
        crate::ui::custom_id::NPAction::Skip { .. } => {
            // Skip one track directly in songbird via modify_queue and skip
            if let Some(handler_lock) = songbird.get(guild_id) {
                let handler = handler_lock.lock().await;
                let _ = handler.queue().skip();
            }

            let _ = interaction
                .create_response(
                    http,
                    serenity::builder::CreateInteractionResponse::Acknowledge,
                )
                .await;
        }
    }
}
