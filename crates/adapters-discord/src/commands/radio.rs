use std::sync::Arc;

use serenity::all::Http;
use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;

use adapters_voice::lifecycle::{TrackLifecycleEvent, TrackLifecycleTx};
use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("radio")
        .description("Start radio mode — auto-queue recommendations based on your taste")
}

pub async fn run(
    http: &Arc<Http>,
    interaction: &CommandInteraction,
    guild_state: &Arc<GuildStateMap>,
    lifecycle_tx: &TrackLifecycleTx,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let guild_id = interaction.guild_id.unwrap_or_default();
    let user_id = interaction.user.id.to_string();

    let state_lock = if let Some(s) = guild_state.get(&guild_id) {
        s.clone()
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("Nothing is playing — queue a track first or let radio seed from your taste profile."),
            )
            .await;

        // Even with nothing playing, we can start radio mode — the
        // lifecycle worker will seed from the user's taste profile.
        let state_lock = guild_state
            .entry(guild_id)
            .or_insert_with(|| {
                Arc::new(tokio::sync::Mutex::new(
                    adapters_voice::state::GuildMusicState::new(),
                ))
            })
            .clone();
        let mut state = state_lock.lock().await;
        state.radio_mode = true;
        state.radio_user_id = Some(user_id.clone());
        drop(state);

        let _ = lifecycle_tx.send(TrackLifecycleEvent::RadioRefillNeeded {
            guild_id,
            user_id: user_id.clone(),
            seed_track_id: None,
        });

        return;
    };

    let mut state = state_lock.lock().await;
    if state.radio_mode {
        // Toggle off
        state.radio_mode = false;
        state.radio_user_id = None;
        drop(state);
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("📻 Radio mode **off**."),
            )
            .await;
    } else {
        state.radio_mode = true;
        state.radio_user_id = Some(user_id.clone());

        let trigger_refill = state.meta_queue.len() <= application::RADIO_REFILL_THRESHOLD;
        let current_track = state.meta_queue.front().map(|t| t.track_id);
        drop(state);

        if trigger_refill {
            let _ = lifecycle_tx.send(TrackLifecycleEvent::RadioRefillNeeded {
                guild_id,
                user_id: user_id.clone(),
                seed_track_id: current_track,
            });
        }

        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content(
                    "📻 Radio mode **on** — the queue will auto-fill with recommendations.",
                ),
            )
            .await;
    }
}
