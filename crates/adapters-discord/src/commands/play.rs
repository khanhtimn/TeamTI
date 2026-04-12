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
    youtube_repo: &Arc<dyn application::ports::youtube::YoutubeRepository>,
    ytdlp_port: &Arc<dyn application::ports::ytdlp::YtDlpPort>,
    youtube_worker: &Arc<application::youtube_worker::YoutubeDownloadWorker>,
    ytdlp_binary: &str,
) {
    let _ = interaction.defer_ephemeral(http).await;

    // ── 1. Parse the selected track UUID from the autocomplete value ───────
    let Some(asset_id_str) = extract_query(interaction) else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Invalid selection."),
            )
            .await;
        return;
    };

    let mut resolved_tracks: Vec<Uuid> = Vec::new();

    match Uuid::parse_str(asset_id_str) {
        Ok(id) => resolved_tracks.push(id),
        Err(_) => {
            // ── Check if it is a YouTube URL ──────────────────────────────────────
            if asset_id_str.starts_with("http")
                && (asset_id_str.contains("youtube.com") || asset_id_str.contains("youtu.be"))
            {
                tracing::info!(url = %asset_id_str, "Detected YouTube URL in play command");

                // Check if it's a playlist
                if let Some(_playlist_id) =
                    adapters_ytdlp::extract_youtube_playlist_id(asset_id_str)
                {
                    tracing::info!("Detected YouTube playlist, fetching metadata...");
                    match ytdlp_port.fetch_playlist_metadata(asset_id_str).await {
                        Ok(metas) => {
                            let limited_metas: Vec<_> = metas.into_iter().take(100).collect();
                            match youtube_repo
                                .create_youtube_stubs_batch(&limited_metas)
                                .await
                            {
                                Ok(pairs) => {
                                    for (_vid, tid) in pairs {
                                        resolved_tracks.push(tid);
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to create YouTube playlist stubs");
                                    let _ = interaction
                                        .edit_response(
                                            http,
                                            EditInteractionResponse::new()
                                                .content("Database error creating playlist stubs."),
                                        )
                                        .await;
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to fetch YouTube playlist metadata");
                            let _ = interaction
                                .edit_response(
                                    http,
                                    EditInteractionResponse::new().content(
                                        "Failed to fetch playlist information from YouTube.",
                                    ),
                                )
                                .await;
                            return;
                        }
                    }
                } else if let Some(video_id) =
                    adapters_ytdlp::extract_youtube_video_id(asset_id_str)
                {
                    let canonical = adapters_ytdlp::canonical_youtube_url(&video_id);
                    match ytdlp_port.fetch_video_metadata(&canonical).await {
                        Ok(meta) => match youtube_repo.create_youtube_stub(&meta).await {
                            Ok(id) => resolved_tracks.push(id),
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to create YouTube stub track");
                                let _ = interaction
                                    .edit_response(
                                        http,
                                        EditInteractionResponse::new()
                                            .content("Database error creating YouTube track."),
                                    )
                                    .await;
                                return;
                            }
                        },
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to fetch YouTube metadata");
                            let _ = interaction
                                .edit_response(
                                    http,
                                    EditInteractionResponse::new()
                                        .content("Failed to fetch information from YouTube."),
                                )
                                .await;
                            return;
                        }
                    }
                } else {
                    let _ = interaction
                        .edit_response(
                            http,
                            EditInteractionResponse::new().content("Invalid YouTube URL."),
                        )
                        .await;
                    return;
                }
            } else {
                // Fall back to Tantivy search if the user typed plain text
                match search_port.autocomplete(asset_id_str, 1).await {
                    Ok(results) if !results.is_empty() => resolved_tracks.push(results[0].id),
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
        }
    }

    if resolved_tracks.is_empty() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("No tracks could be resolved."),
            )
            .await;
        return;
    }

    let guild_id = interaction.guild_id.unwrap_or_default();

    // ── 2. Resolve the invoking user's voice channel from the cache ────────
    let user_voice_channel = cache.guild(guild_id).and_then(|g| {
        g.voice_states
            .get(&interaction.user.id)
            .and_then(|vs| vs.channel_id)
    });

    let Some(channel_id) = user_voice_channel else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("You must be in a voice channel to use this command."),
            )
            .await;
        return;
    };

    let mut added_tracks = 0;
    let mut last_title = String::new();

    for track_id in resolved_tracks {
        // ── 3. Look up track metadata from the database ────────────────────────
        let Ok(Some(track)) = track_repo.find_by_id(track_id).await else {
            continue;
        };

        if added_tracks == 0 {
            last_title = track.title.clone();
        }

        let mut queued = QueuedTrack::from(&track);
        queued.added_by = interaction.user.id.to_string();
        queued.source = adapters_voice::state::QueueSource::Manual;

        // ── 3b. Trigger YouTube background download if necessary ───────────────
        if track.source == "youtube" && track.blob_location.is_none() {
            let video_id = track.youtube_video_id.clone().unwrap_or_default();
            let youtube_url = format!("https://www.youtube.com/watch?v={video_id}");

            let uploader = track.youtube_uploader.as_deref().unwrap_or("unknown");
            let title = &track.title;
            let blob_path = adapters_ytdlp::youtube_blob_path(uploader, title, &video_id);

            queued.youtube_blob_path = Some(blob_path.clone());

            let job = domain::NewYoutubeDownloadJob {
                video_id: video_id.clone(),
                track_id: track.id,
                url: youtube_url.clone(),
            };

            if let Err(e) = youtube_repo.upsert_download_job(&job).await {
                tracing::warn!(error = %e, "Failed to upsert download job");
            }

            youtube_worker.schedule(video_id, blob_path);
        }

        // ── 4. Handle channel move (only needed on first track) ────────────────
        if added_tracks == 0 {
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

            // ── 5. Join the voice channel ──────────────────────────────────────────
            if let Err(e) = join_channel(songbird, guild_id, channel_id).await {
                tracing::error!(guild_id = %guild_id, error = %e, "failed to join voice channel");
                let _ = interaction
                    .edit_response(
                        http,
                        EditInteractionResponse::new()
                            .content("Failed to join your voice channel."),
                    )
                    .await;
                return;
            }
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

        // ── 7. Enqueue into Songbird ───────────────────────────────────────────
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
            ytdlp_binary,
        )
        .await
        {
            Ok(_) => {
                added_tracks += 1;
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
                    "failed to enqueue track"
                );
            }
        }
    }

    if added_tracks == 1 {
        let msg = format!("✅ Added **{last_title}** to the queue");
        let _ = interaction
            .edit_response(http, EditInteractionResponse::new().content(msg))
            .await;
    } else if added_tracks > 1 {
        let msg = format!("✅ Added **{added_tracks} tracks** to the queue");
        let _ = interaction
            .edit_response(http, EditInteractionResponse::new().content(msg))
            .await;
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Failed to queue any tracks."),
            )
            .await;
    }
}
