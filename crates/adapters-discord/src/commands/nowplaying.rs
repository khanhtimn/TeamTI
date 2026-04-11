use std::sync::Arc;
use std::time::Duration;

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
#[allow(clippy::too_many_arguments)]
pub async fn handle_np_button(
    http: &Arc<serenity::all::Http>,
    cache: &Arc<serenity::all::Cache>,
    interaction: &serenity::model::application::ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
    media_root: &std::path::Path,
    auto_leave_secs: u64,
    lifecycle_tx: adapters_voice::lifecycle::TrackLifecycleTx,
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
                    let paused_ms = i64::try_from(pa.elapsed().as_millis()).unwrap_or_default();
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
        crate::ui::custom_id::NPAction::Prev { .. } => {
            let state_lock = match guild_state_map.get(&guild_id) {
                Some(s) => s.clone(),
                None => return,
            };

            let play_ms = {
                let state = state_lock.lock().await;
                state.actual_play_ms()
            };

            // Rewind logic (> 5s)
            if play_ms > 5000 {
                {
                    let mut state = state_lock.lock().await;
                    state.track_started_at = Some(std::time::Instant::now());
                    state.total_paused_ms = 0;
                }
                if let Some(handler_lock) = songbird.get(guild_id)
                    && let Some(curr) = handler_lock.lock().await.queue().current()
                {
                    let _ = curr.seek(Duration::from_secs(0));
                }
            } else {
                // Reverse track logic (<= 5s)
                let mut state = state_lock.lock().await;
                if state.history.is_empty() {
                    // Fallback to rewinding if no history exists even if < 5s
                    state.track_started_at = Some(std::time::Instant::now());
                    state.total_paused_ms = 0;
                    drop(state);
                    if let Some(handler_lock) = songbird.get(guild_id)
                        && let Some(curr) = handler_lock.lock().await.queue().current()
                    {
                        let _ = curr.seek(Duration::from_secs(0));
                    }
                } else {
                    let mut target = state.history.pop_back().unwrap();
                    target.songbird_uuid = None;

                    let mut current_clone = state.meta_queue.front().cloned();
                    if let Some(c) = current_clone.as_mut() {
                        c.songbird_uuid = None;
                    }

                    state.suppress_history_push = true; // Signal event handler to not duplicate current to history

                    // Push target to meta_queue so it matches Songbird's queue size before reordering
                    state.meta_queue.push_back(target.clone());
                    drop(state);

                    // Re-enqueue target (places at end of Songbird / meta_queue)
                    let _ = adapters_voice::player::enqueue_track(
                        songbird,
                        guild_id,
                        &target,
                        media_root,
                        http,
                        cache,
                        auto_leave_secs,
                        guild_state_map,
                        lifecycle_tx.clone(),
                    )
                    .await;

                    // Re-enqueue current track to retain position (places at end of Songbird / meta_queue)
                    if let Some(ref curr_clone) = current_clone {
                        {
                            let mut state = state_lock.lock().await;
                            state.meta_queue.push_back(curr_clone.clone());
                        }
                        let _ = adapters_voice::player::enqueue_track(
                            songbird,
                            guild_id,
                            curr_clone,
                            media_root,
                            http,
                            cache,
                            auto_leave_secs,
                            guild_state_map,
                            lifecycle_tx.clone(),
                        )
                        .await;
                    }

                    // Extract the latest 2 additions and position them at Index 1 & Index 2
                    {
                        let mut state = state_lock.lock().await;
                        if state.meta_queue.len() > 1 && current_clone.is_some() {
                            // Enqueued target, then curr_clone. So tail is [..., target, curr_clone]
                            let curr = state.meta_queue.pop_back().unwrap();
                            let tgt = state.meta_queue.pop_back().unwrap();
                            state.meta_queue.insert(1, tgt);
                            state.meta_queue.insert(2, curr);
                        } else if state.meta_queue.len() > 1 {
                            // Only target enqueued
                            let tgt = state.meta_queue.pop_back().unwrap();
                            state.meta_queue.insert(1, tgt);
                        }
                    }

                    {
                        let state = state_lock.lock().await;
                        let uuids_in_order: Vec<_> = state
                            .meta_queue
                            .iter()
                            .filter_map(|t| t.songbird_uuid)
                            .collect();

                        if let Some(handler_lock) = songbird.get(guild_id) {
                            let handler = handler_lock.lock().await;
                            handler.queue().modify_queue(|q| {
                                let mut map: std::collections::HashMap<_, _> =
                                    q.drain(..).map(|h| (h.handle().uuid(), h)).collect();
                                for target_uuid in uuids_in_order {
                                    if let Some(handle) = map.remove(&target_uuid) {
                                        q.push_back(handle);
                                    }
                                }
                                for (_, handle) in map {
                                    q.push_back(handle);
                                }
                            });
                            let _ = handler.queue().skip();
                        }
                    }
                }
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
