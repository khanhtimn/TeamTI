use std::sync::Arc;

use serenity::builder::{
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, CommandOptionType, ResolvedValue};
use serenity::model::permissions::Permissions;

use adapters_voice::lifecycle::TrackLifecycleTx;
use adapters_voice::state_map::GuildStateMap;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("skip")
        .description("Skip to the next track or to a specific position")
        .default_member_permissions(Permissions::SEND_MESSAGES)
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "target",
                "Position number or track name to skip to",
            )
            .required(false)
            .set_autocomplete(true),
        )
}

pub async fn autocomplete(
    http: &serenity::all::Http,
    interaction: &CommandInteraction,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let guild_id = interaction.guild_id.unwrap_or_default();

    let choices = match guild_state_map.get(&guild_id) {
        Some(state_lock) => {
            let state = state_lock.lock().await;
            state
                .meta_queue
                .iter()
                .enumerate()
                .skip(1) // skip the currently playing track
                .take(25)
                .map(|(i, track)| {
                    let label = format_queue_choice(i, track);
                    serenity::builder::AutocompleteChoice::new(label, i.to_string())
                })
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    let _ = interaction
        .create_response(
            http,
            serenity::builder::CreateInteractionResponse::Autocomplete(
                CreateAutocompleteResponse::new().set_choices(choices),
            ),
        )
        .await;
}

/// Execute the /skip command.
/// - No argument: skip current track (advance to next).
/// - Numeric N: skip to position N (discard tracks before N).
/// - String from autocomplete: parsed as position index.
pub async fn run(
    http: &Arc<serenity::all::Http>,
    interaction: &CommandInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
    lifecycle_tx: &TrackLifecycleTx,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = if let Some(state) = guild_state_map.get(&guild_id) {
        state.clone()
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("The bot isn't in a voice channel. Use /play to start."),
            )
            .await;
        return;
    };

    // Parse optional target argument
    let target_str = interaction
        .data
        .options()
        .iter()
        .find(|o| o.name == "target")
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => Some(s.to_string()),
            _ => None,
        });

    let mut state = state_lock.lock().await;

    if state.meta_queue.is_empty() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Nothing is currently playing."),
            )
            .await;
        return;
    }

    // Determine skip target position
    let skip_to = match target_str {
        None => 1, // skip current → advance to next (position 1)
        Some(ref s) => {
            // Try parsing as integer position first
            if let Ok(n) = s.parse::<usize>() {
                n
            } else {
                let _ = interaction
                    .edit_response(
                        http,
                        EditInteractionResponse::new()
                            .content(format!("Position {s} doesn't exist in the queue.")),
                    )
                    .await;
                return;
            }
        }
    };

    if skip_to >= state.meta_queue.len() {
        // Skipping past the end — clear queue
        if state.meta_queue.len() <= 1 {
            // Only 1 track (the current one). Use skip() to trigger the
            // normal TrackEventHandler flow which posts "Queue Ended",
            // starts the auto-leave timer, and emits lifecycle events.
            state.cancel_np_update();
            drop(state);

            if let Some(handler_lock) = songbird.get(guild_id) {
                let handler = handler_lock.lock().await;
                let _ = handler.queue().skip();
            }

            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("⏭ Skipped. No more tracks in queue."),
                )
                .await;
            return;
        }

        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content(format!("Position {skip_to} doesn't exist in the queue.")),
            )
            .await;
        return;
    }

    // Get the name of the track we're skipping to
    let target_name = state.meta_queue[skip_to].title.clone();

    state.cancel_np_update();

    // ── Drain intermediate tracks from meta_queue ourselves ─────────
    // We remove positions 1..skip_to from meta_queue (the tracks between
    // the currently-playing track and the skip target). We emit TrackEnded
    // for each with zero play time so listen events are properly closed.
    for _ in 1..skip_to {
        if state.meta_queue.len() > 1 {
            let skipped = state.meta_queue.remove(1).unwrap();
            let _ = lifecycle_tx.send(adapters_voice::lifecycle::TrackLifecycleEvent::TrackEnded {
                guild_id,
                track_id: skipped.track_id,
                track_duration_ms: skipped.duration_ms,
                play_duration_ms: 0, // never played
            });
        }
    }
    drop(state);

    // Now remove the same intermediate entries from Songbird's queue.
    // These are Queued (not Playing) entries so dropping them does NOT
    // fire TrackEvent::End — only the Playing entry does on skip().
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            for _ in 1..skip_to {
                if q.len() > 1 {
                    q.remove(1);
                }
            }
        });

        // skip() stops the currently-playing track (position 0), which
        // fires exactly ONE TrackEvent::End → TrackEventHandler.act()
        // pops position 0 from meta_queue and posts the new NP embed.
        let _ = handler.queue().skip();
    }

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new().content(format!("⏭ Skipped to **{target_name}**.")),
        )
        .await;
}

/// Format a queue entry for autocomplete display: "N. Title — Artist"
fn format_queue_choice(index: usize, track: &adapters_voice::state::QueuedTrack) -> String {
    let raw = format!("{}. {} — {}", index, track.title, track.artist);
    if raw.len() > 100 {
        let end = raw.floor_char_boundary(97);
        format!("{}...", &raw[..end])
    } else {
        raw
    }
}
