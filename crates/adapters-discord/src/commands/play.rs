use std::path::Path;
use std::sync::Arc;

use serenity::all::{Cache, Http};
use serenity::builder::{
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, CommandOptionType, ResolvedValue};
use serenity::model::id::ChannelId;
use uuid::Uuid;

use adapters_persistence::repositories::track_repository::PgTrackRepository;
use adapters_voice::player::{enqueue_track, join_channel};
use adapters_voice::state::QueuedTrack;
use adapters_voice::state_map::GuildStateMap;
use adapters_voice::track_event_handler::post_now_playing;
use application::ports::repository::TrackRepository;
use application::ports::search::TrackSearchPort;

fn extract_query(cmd: &CommandInteraction) -> Option<&str> {
    for option in &cmd.data.options() {
        if option.name == "query"
            && let ResolvedValue::String(s) = option.value
        {
            return Some(s);
        }
    }
    None
}

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("play")
        .description("Search and queue a track")
        .add_option(
            CreateCommandOption::new(CommandOptionType::String, "query", "Track title to search")
                .required(true)
                .set_autocomplete(true),
        )
}

pub async fn autocomplete(
    http: &serenity::all::Http,
    interaction: &CommandInteraction,
    track_repo: &Arc<PgTrackRepository>,
) {
    let mut query = "";
    for option in &interaction.data.options {
        if option.name == "query"
            && let Some(s) = option.value.as_str()
        {
            query = s;
        }
    }

    match track_repo.search(query, 25).await {
        Ok(results) => {
            let choices: Vec<_> = results
                .into_iter()
                .map(|r| {
                    let raw = if let Some(artist) = &r.artist_display {
                        format!("{} — {}", r.title, artist)
                    } else {
                        r.title.clone()
                    };
                    let display = if raw.len() > 100 {
                        // Unicode-safe truncation: floor_char_boundary finds
                        // the largest char-aligned byte index <= 97.
                        let end = raw.floor_char_boundary(97);
                        format!("{}...", &raw[..end])
                    } else {
                        raw
                    };
                    serenity::builder::AutocompleteChoice::new(display, r.id.to_string())
                })
                .collect();

            let _ = interaction
                .create_response(
                    http,
                    serenity::builder::CreateInteractionResponse::Autocomplete(
                        CreateAutocompleteResponse::new().set_choices(choices),
                    ),
                )
                .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Autocomplete search failed");
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    http: &Arc<Http>,
    cache: &Arc<Cache>,
    interaction: &CommandInteraction,
    track_repo: &Arc<PgTrackRepository>,
    media_root: &Path,
    auto_leave_secs: u64,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    // ── 1. Parse the selected track UUID from the autocomplete value ───────
    let asset_id_str = match extract_query(interaction) {
        Some(s) => s,
        None => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Invalid selection."),
                )
                .await;
            return;
        }
    };

    let track_id = match Uuid::parse_str(asset_id_str) {
        Ok(id) => id,
        Err(_) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content("Invalid track ID — please use the autocomplete list."),
                )
                .await;
            return;
        }
    };

    let guild_id = interaction.guild_id.unwrap_or_default();

    // ── 2. Resolve the invoking user's voice channel from the cache ────────
    let user_voice_channel = cache.guild(guild_id).and_then(|g| {
        g.voice_states
            .get(&interaction.user.id)
            .and_then(|vs| vs.channel_id)
    });

    let channel_id = match user_voice_channel {
        Some(ch) => ch,
        None => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content("You must be in a voice channel to use this command."),
                )
                .await;
            return;
        }
    };

    // ── 3. Look up track metadata from the database ────────────────────────
    let track = match track_repo.find_by_id(track_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Track not found in the library."),
                )
                .await;
            return;
        }
        Err(e) => {
            tracing::error!(
                guild_id  = %guild_id,
                track_id  = %track_id,
                error     = %e,
                operation = "play.db_lookup",
                "database error looking up track"
            );
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Database error fetching track."),
                )
                .await;
            return;
        }
    };

    let queued = QueuedTrack::from(&track);

    // ── 4. Handle channel move ─────────────────────────────────────────────
    // If the bot is already in a *different* channel, clear our meta_queue
    // and stop Songbird's queue before moving. Songbird's Driver is reset on
    // rejoin, so the old TrackQueue is lost regardless.
    {
        let existing_channel = guild_state_map.get(&guild_id).and_then(|s| {
            // Don't block — try_lock to avoid deadlock with other commands
            s.try_lock().ok().and_then(|state| state.voice_channel_id)
        });

        if let Some(existing) = existing_channel
            && existing != channel_id
        {
            tracing::info!(
                guild_id       = %guild_id,
                from_channel   = %existing,
                to_channel     = %channel_id,
                operation      = "play.channel_move",
                "channel move detected — clearing queue before rejoin"
            );
            // Stop Songbird's queue (it will be destroyed on rejoin anyway)
            if let Some(handler_lock) = songbird.get(guild_id) {
                handler_lock.lock().await.queue().stop();
            }
            // Clear our metadata mirror
            if let Some(state_lock) = guild_state_map.get(&guild_id) {
                let mut state = state_lock.lock().await;
                state.meta_queue.clear();
                state.cancel_auto_leave();
            }
        }
    }

    // ── 5. Join the voice channel (no-op if already in the same channel) ───
    if let Err(e) = join_channel(songbird, guild_id, channel_id).await {
        tracing::error!(
            guild_id   = %guild_id,
            channel_id = %channel_id,
            error      = %e,
            operation  = "play.join",
            "failed to join voice channel"
        );
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Failed to join your voice channel."),
            )
            .await;
        return;
    }

    // ── 6. Update guild state ──────────────────────────────────────────────
    let state_lock = guild_state_map
        .entry(guild_id)
        .or_insert_with(|| {
            Arc::new(tokio::sync::Mutex::new(
                adapters_voice::state::GuildMusicState::new(),
            ))
        })
        .clone();

    {
        let mut state = state_lock.lock().await;
        state.voice_channel_id = Some(channel_id);
        state.text_channel_id = Some(ChannelId::new(interaction.channel_id.get()));
        state.cancel_auto_leave();
        // Push metadata to our parallel queue *before* enqueue_track so the
        // count is accurate when TrackEventHandler fires.
        state.meta_queue.push_back(queued.clone());
    }

    // ── 7. Enqueue into Songbird's builtin queue ───────────────────────────
    match enqueue_track(
        songbird,
        guild_id,
        &queued,
        media_root,
        http,
        auto_leave_secs,
        guild_state_map,
    )
    .await
    {
        Ok(_) => {
            let queue_pos = {
                let state = state_lock.lock().await;
                state.meta_queue.len()
            };
            let msg = if queue_pos == 1 {
                // Post the "Now Playing" embed for the first track
                let text_channel = ChannelId::new(interaction.channel_id.get());
                post_now_playing(
                    http,
                    text_channel,
                    guild_id,
                    &state_lock,
                    Some(&queued),
                    None, // no existing message to edit
                )
                .await;
                format!("▶ Now playing **{}**", queued.title)
            } else {
                format!(
                    "✅ Added **{}** to the queue (position {})",
                    queued.title, queue_pos
                )
            };
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
        }
        Err(e) => {
            // Roll back the metadata push on failure
            {
                let mut state = state_lock.lock().await;
                state.meta_queue.pop_back();
            }
            tracing::error!(
                guild_id  = %guild_id,
                track_id  = %queued.track_id,
                error     = %e,
                operation = "play.enqueue",
                "failed to enqueue track"
            );
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Failed to queue the track."),
                )
                .await;
        }
    }
}
