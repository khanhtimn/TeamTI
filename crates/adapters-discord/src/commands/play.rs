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
use adapters_voice::lifecycle::TrackLifecycleTx;
use adapters_voice::player::{enqueue_track, join_channel};
use adapters_voice::state::QueuedTrack;
use adapters_voice::state_map::GuildStateMap;

use application::ports::recommendation::RecommendationPort;
use application::ports::repository::TrackRepository;
use application::ports::search::TrackSearchPort;
use application::ports::user_library::UserLibraryPort;

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
    search_port: &Arc<dyn TrackSearchPort>,
    user_library_port: &Arc<dyn UserLibraryPort>,
    recommendation_port: &Arc<dyn RecommendationPort>,
) {
    let mut query = "";
    for option in &interaction.data.options {
        if option.name == "query"
            && let Some(s) = option.value.as_str()
        {
            query = s;
        }
    }

    if query.is_empty() {
        // Empty query: mix recent history + favourites + recommendations
        let user_id = interaction.user.id.to_string();
        let mut choices: Vec<serenity::builder::AutocompleteChoice<'_>> = Vec::with_capacity(25);

        // 1. Last 8 distinct tracks from listen history
        if let Ok(recent) = user_library_port.recent_history(&user_id, 8).await {
            for r in &recent {
                if choices.len() >= 25 {
                    break;
                }
                let display = format_track_display(r);
                choices.push(serenity::builder::AutocompleteChoice::new(
                    format!("🕐 {display}"),
                    r.id.to_string(),
                ));
            }
        }

        // 2. Up to 8 favourites not already in choices
        if choices.len() < 25 {
            let existing_ids: Vec<Uuid> = choices
                .iter()
                .filter_map(|c| match &c.value {
                    serenity::builder::AutocompleteValue::String(s) => Uuid::parse_str(s).ok(),
                    _ => None,
                })
                .collect();

            if let Ok(favs) = user_library_port.list_favourites(&user_id, 0, 8).await {
                for f in &favs.tracks {
                    if choices.len() >= 25 {
                        break;
                    }
                    if existing_ids.contains(&f.id) {
                        continue;
                    }
                    let display = format_track_display(f);
                    choices.push(serenity::builder::AutocompleteChoice::new(
                        format!("❤️ {display}"),
                        f.id.to_string(),
                    ));
                }
            }
        }

        // 3. Fill remaining with recommendations
        if choices.len() < 25 {
            let exclude_ids: Vec<Uuid> = choices
                .iter()
                .filter_map(|c| match &c.value {
                    serenity::builder::AutocompleteValue::String(s) => Uuid::parse_str(s).ok(),
                    _ => None,
                })
                .collect();
            let remaining = 25 - choices.len();

            if let Ok(recs) = recommendation_port
                .recommend(
                    &user_id,
                    None,
                    None,
                    domain::MoodWeight::default(),
                    &exclude_ids,
                    remaining,
                )
                .await
            {
                for r in &recs {
                    if choices.len() >= 25 {
                        break;
                    }
                    let display = format_track_display(r);
                    choices.push(serenity::builder::AutocompleteChoice::new(
                        format!("✨ {display}"),
                        r.id.to_string(),
                    ));
                }
            }
        }

        let _ = interaction
            .create_response(
                http,
                serenity::builder::CreateInteractionResponse::Autocomplete(
                    CreateAutocompleteResponse::new().set_choices(choices),
                ),
            )
            .await;
    } else {
        // Normal search autocomplete
        match search_port.autocomplete(query, 25).await {
            Ok(results) => {
                let choices: Vec<_> = results
                    .into_iter()
                    .map(|r| {
                        let display = format_track_display(&r);
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
}

fn format_track_display(r: &domain::track::TrackSummary) -> String {
    let raw = if let Some(artist) = &r.artist_display {
        format!("{} — {}", r.title, artist)
    } else {
        r.title.clone()
    };
    if raw.len() > 100 {
        let end = raw.floor_char_boundary(97);
        format!("{}...", &raw[..end])
    } else {
        raw
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
    lifecycle_tx: &TrackLifecycleTx,
    search_port: &Arc<dyn TrackSearchPort>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    // ── 1. Parse the selected track UUID from the autocomplete value ───────
    let asset_id_str = if let Some(s) = extract_query(interaction) {
        s
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Invalid selection."),
            )
            .await;
        return;
    };

    let track_id = match Uuid::parse_str(asset_id_str) {
        Ok(id) => id,
        Err(_) => {
            // Fall back to Tantivy search if the user typed text directly
            match search_port.autocomplete(asset_id_str, 1).await {
                Ok(results) if !results.is_empty() => results[0].id,
                _ => {
                    let _ = interaction
                        .edit_response(
                            http,
                            EditInteractionResponse::new()
                                .content("No matching track found for your search."),
                        )
                        .await;
                    return;
                }
            }
        }
    };

    let guild_id = interaction.guild_id.unwrap_or_default();

    // ── 2. Resolve the invoking user's voice channel from the cache ────────
    let user_voice_channel = cache.guild(guild_id).and_then(|g| {
        g.voice_states
            .get(&interaction.user.id)
            .and_then(|vs| vs.channel_id)
    });

    let channel_id = if let Some(ch) = user_voice_channel {
        ch
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("You must be in a voice channel to use this command."),
            )
            .await;
        return;
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

    let mut queued = QueuedTrack::from(&track);
    queued.added_by = interaction.user.id.to_string();
    queued.source = adapters_voice::state::QueueSource::Manual;

    // ── 4. Handle channel move ─────────────────────────────────────────────
    {
        let existing_channel = guild_state_map
            .get(&guild_id)
            .and_then(|s| s.try_lock().ok().and_then(|state| state.voice_channel_id));

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
            if let Some(handler_lock) = songbird.get(guild_id) {
                handler_lock.lock().await.queue().stop();
            }
            if let Some(state_lock) = guild_state_map.get(&guild_id) {
                let mut state = state_lock.lock().await;
                state.meta_queue.clear();
                state.cancel_auto_leave();
            }
        }
    }

    // ── 5. Join the voice channel ──────────────────────────────────────────
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
        state.meta_queue.push_back(queued.clone());
    }

    // ── 9. Enqueue into Songbird ───────────────────────────────────────────
    match enqueue_track(
        songbird,
        guild_id,
        &queued,
        media_root,
        http,
        cache,
        auto_leave_secs,
        guild_state_map,
        lifecycle_tx.clone(),
    )
    .await
    {
        Ok(_) => {
            let queue_pos = {
                let state = state_lock.lock().await;
                state.meta_queue.len()
            };
            let msg = if queue_pos == 1 {
                format!("▶ Now playing **{}**", queued.title)
            } else if queue_pos == 2 {
                format!("✅ Added **{}** — up next", queued.title)
            } else {
                format!(
                    "✅ Added **{}** — {} tracks away",
                    queued.title,
                    queue_pos - 1
                )
            };
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
        }
        Err(e) => {
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
