use serenity::all::{Cache, Http};
use serenity::builder::{
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, CommandOptionType, ResolvedValue};
use serenity::model::id::ChannelId;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

use adapters_persistence::repositories::track_repository::PgTrackRepository;
use adapters_voice::lifecycle::TrackLifecycleTx;
use adapters_voice::player::{enqueue_track, join_channel};
use adapters_voice::state::QueuedTrack;
use adapters_voice::state_map::GuildStateMap;

use application::ports::MusicSearchPort;
use application::ports::recommendation::RecommendationPort;
use application::ports::repository::TrackRepository;
use application::ports::user_library::UserLibraryPort;
use domain::autocomplete::SubmissionValue;
use domain::search::SearchFilter;

pub struct UserAutocompleteState {
    pub previous_query: String,
    pub triggered: bool,
    pub last_seen: tokio::time::Instant,
}

pub type AutocompleteCache = moka::future::Cache<String, Vec<domain::search::SearchResult>>;

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

#[allow(clippy::too_many_arguments)]
pub async fn autocomplete(
    http: &serenity::all::Http,
    interaction: &CommandInteraction,
    search_port: &Arc<dyn MusicSearchPort>,
    user_library_port: &Arc<dyn UserLibraryPort>,
    recommendation_port: &Arc<dyn RecommendationPort>,
    ac_cache: &Arc<AutocompleteCache>,
    user_state: &Arc<dashmap::DashMap<String, UserAutocompleteState>>,
    youtube_worker: &Arc<application::youtube_search_worker::YoutubeSearchWorker>,
    youtube_repo: &Arc<dyn application::ports::youtube::YoutubeRepository>,
    youtube_search_repo: &Arc<dyn application::ports::repository::YoutubeSearchRepository>,
    ytdlp_port: &Arc<dyn application::ports::ytdlp::YtDlpPort>,
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
        let user_id = interaction.user.id.to_string();

        let limit = 25;
        let mut filter = SearchFilter::All;
        let mut actual_query = query;

        if let Some(stripped) = query.strip_prefix("yt:") {
            actual_query = stripped.trim();
            filter = SearchFilter::YoutubeOnly;

            // M5: For yt: mode, always trigger immediate background fetch
            if !actual_query.is_empty() && !ac_cache.contains_key(actual_query) {
                let worker = youtube_worker.clone();
                let q = actual_query.to_string();
                let cache = ac_cache.clone();
                tokio::spawn(async move {
                    worker.fetch_and_cache(q.clone(), cache).await;
                });
            }
        } else if !query.is_empty() {
            // M4: YouTube URL paste — resolve preview from Tantivy first,
            // then fall back to DB only if Tantivy hasn't indexed it yet
            if actual_query.starts_with("http")
                && (actual_query.contains("youtube.com") || actual_query.contains("youtu.be"))
                && let Some(vid) = adapters_ytdlp::extract_youtube_video_id(actual_query)
            {
                // Try Tantivy first — avoids DB round-trip in hot path
                let mut preview_title = format!("▶ YouTube Video ({vid})");
                let mut preview_subtitle = String::new();
                let mut found = false;

                // Check if Tantivy has this video indexed (either as track or search cache)
                if let Ok(tantivy_results) =
                    search_port.autocomplete(&vid, SearchFilter::All, 1).await
                    && let Some(r) = tantivy_results.first()
                    && r.youtube_video_id.as_deref() == Some(&*vid)
                {
                    preview_title = format!("▶ {}", r.title);
                    preview_subtitle = r
                        .artist_display
                        .clone()
                        .or_else(|| r.uploader.clone())
                        .unwrap_or_default();
                    found = true;
                }

                // DB fallback for URLs not yet in Tantivy
                if !found {
                    if let Ok(Some(track)) = youtube_repo.find_track_by_video_id(&vid).await {
                        preview_title = format!("▶ {}", track.title);
                        preview_subtitle = track.artist_display.unwrap_or_default();
                        found = true;
                    } else if let Ok(Some(cache_row)) = youtube_search_repo
                        .find_search_cache_by_video_id(&vid)
                        .await
                    {
                        preview_title = format!("▶ {}", cache_row.title);
                        preview_subtitle = cache_row.uploader.unwrap_or_default();
                        found = true;
                    }
                }

                // yt-dlp fetch fallback — video is completely unknown.
                // Use a 2.5s timeout to stay within Discord's 3s autocomplete window.
                if !found {
                    let canonical = adapters_ytdlp::canonical_youtube_url(&vid);
                    if let Ok(Ok(meta)) = tokio::time::timeout(
                        std::time::Duration::from_millis(2500),
                        ytdlp_port.fetch_video_metadata(&canonical),
                    )
                    .await
                    {
                        let title = meta
                            .track_title
                            .as_deref()
                            .or(meta.title.as_deref())
                            .unwrap_or("Unknown Title");
                        let artist = meta
                            .artist
                            .as_deref()
                            .or(meta.uploader.as_deref())
                            .unwrap_or_default();

                        preview_title = format!("▶ {title}");
                        preview_subtitle = artist.to_string();

                        // Cache in DB so run() finds it without re-fetching
                        let _ = youtube_search_repo
                            .upsert_search_result("__url_paste__", &meta)
                            .await;
                    }
                }

                let subtitle_str = if preview_subtitle.is_empty() {
                    String::new()
                } else {
                    format!(" — {preview_subtitle}")
                };

                let display = truncate_smart(
                    "",
                    &preview_title,
                    if subtitle_str.is_empty() {
                        None
                    } else {
                        Some(&subtitle_str)
                    },
                    100,
                );

                let choices = vec![serenity::builder::AutocompleteChoice::new(
                    display,
                    SubmissionValue::YoutubeVideoId(vid).serialize(),
                )];
                let _ = interaction
                    .create_response(
                        http,
                        serenity::builder::CreateInteractionResponse::Autocomplete(
                            CreateAutocompleteResponse::new().set_choices(choices),
                        ),
                    )
                    .await;
                return;
            }

            // Eagerly trigger YouTube background fetch (once per query).
            // The old C6 500ms stability gate was broken — Discord only fires
            // autocomplete on keystrokes, so the timer could never be checked
            // after the user stopped typing. Now we fire immediately on the
            // first unseen query. DashSet in_flight in YoutubeSearchWorker
            // prevents duplicate fetches.
            if !ac_cache.contains_key(query) {
                if let Some(state) = user_state.get(&user_id) {
                    if state.previous_query != query || !state.triggered {
                        drop(state);
                        user_state.insert(
                            user_id.clone(),
                            UserAutocompleteState {
                                previous_query: query.to_string(),
                                triggered: true,
                                last_seen: tokio::time::Instant::now(),
                            },
                        );
                        let worker = youtube_worker.clone();
                        let q = query.to_string();
                        let cache = ac_cache.clone();
                        tokio::spawn(async move {
                            worker.fetch_and_cache(q, cache).await;
                        });
                    }
                } else {
                    user_state.insert(
                        user_id.clone(),
                        UserAutocompleteState {
                            previous_query: query.to_string(),
                            triggered: true,
                            last_seen: tokio::time::Instant::now(),
                        },
                    );
                    let worker = youtube_worker.clone();
                    let q = query.to_string();
                    let cache = ac_cache.clone();
                    tokio::spawn(async move {
                        worker.fetch_and_cache(q, cache).await;
                    });
                }
            }
        }

        // C2/O1: Single-source Tantivy query.
        let mut final_results = match search_port
            .autocomplete(actual_query, filter.clone(), limit)
            .await
        {
            Ok(results) => results,
            Err(e) => {
                tracing::warn!(error = %e, "Autocomplete search failed");
                Vec::new()
            }
        };

        // O2: Backfill from moka cache if Tantivy returned sparse results
        if final_results.len() < 5
            && let Some(cached) = ac_cache.get(actual_query).await
        {
            // Deduplicate against existing results
            for c in cached {
                if final_results.len() >= limit {
                    break;
                }
                let dominated = final_results
                    .iter()
                    .any(|existing| existing.youtube_video_id == c.youtube_video_id);
                if !dominated {
                    final_results.push(c);
                }
            }
        }

        let mut choices: Vec<_> = final_results
            .into_iter()
            .filter_map(|r| {
                let val = if let Some(tid) = r.track_id {
                    SubmissionValue::TrackId(tid).serialize()
                } else if let Some(vid) = &r.youtube_video_id {
                    SubmissionValue::YoutubeVideoId(vid.clone()).serialize()
                } else {
                    return None;
                };
                let display = format_search_result(&r);
                Some(serenity::builder::AutocompleteChoice::new(display, val))
            })
            .collect();

        // Always-available YouTube search sentinel.
        // When results are sparse, offer a direct YouTube search option so the
        // user always has something to select. On submission, run() will do a
        // live ytsearch1:{query} via yt-dlp and play the top result.
        if choices.len() < 5 && !actual_query.is_empty() && filter != SearchFilter::LocalOnly {
            let sentinel_display =
                truncate_display(format!("🔎 Search YouTube: \"{actual_query}\""));
            choices.push(serenity::builder::AutocompleteChoice::new(
                sentinel_display,
                SubmissionValue::YoutubeSearch(actual_query.to_string()).serialize(),
            ));
        }

        let _ = interaction
            .create_response(
                http,
                serenity::builder::CreateInteractionResponse::Autocomplete(
                    CreateAutocompleteResponse::new().set_choices(choices),
                ),
            )
            .await;
    }
}

fn truncate_display(raw: String) -> String {
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() > 97 {
        let truncated: String = chars.into_iter().take(97).collect();
        format!("{truncated}...")
    } else {
        raw
    }
}

fn format_duration(ms: i64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let remainder_secs = secs % 60;
    format!("{mins}:{remainder_secs:02}")
}

fn truncate_smart(prefix: &str, title: &str, suffix: Option<&str>, max_len: usize) -> String {
    let prefix_chars: Vec<char> = prefix.chars().collect();
    let title_chars: Vec<char> = title.chars().collect();
    let suffix_chars: Vec<char> = suffix.map(|s| s.chars().collect()).unwrap_or_default();

    let total_len = prefix_chars.len() + title_chars.len() + suffix_chars.len();
    if total_len <= max_len {
        let mut res = String::new();
        res.push_str(prefix);
        res.push_str(title);
        if let Some(s) = suffix {
            res.push_str(s);
        }
        return res;
    }

    // Try dropping the suffix entirely if keeping the title fits (preserves the most important info)
    let title_req = prefix_chars.len() + title_chars.len();
    if title_req <= max_len {
        let mut res = String::new();
        res.push_str(prefix);
        res.push_str(title);
        return res;
    }

    // Still too long: drop suffix and truncate the title to fit within `max_len`
    let available_title_len = max_len.saturating_sub(prefix_chars.len() + 3); // 3 for "..."
    if available_title_len == 0 {
        return prefix.chars().take(max_len).collect();
    }

    let truncated_title: String = title_chars.into_iter().take(available_title_len).collect();
    format!("{prefix}{truncated_title}...")
}

fn format_track_display(r: &domain::track::TrackSummary) -> String {
    let mut suffix = String::new();
    if let Some(artist) = &r.artist_display {
        suffix.push_str(" — ");
        suffix.push_str(artist);
    }
    truncate_smart(
        "",
        &r.title,
        if suffix.is_empty() {
            None
        } else {
            Some(&suffix)
        },
        100,
    )
}

fn format_search_result(r: &domain::search::SearchResult) -> String {
    let prefix = match r.source.as_str() {
        "youtube" => "📺 ",
        "youtube_search" => "🔎 ",
        _ => "🎵 ",
    };

    let mut suffix = String::new();
    if let Some(artist) = &r.artist_display.as_deref().or(r.uploader.as_deref()) {
        suffix.push_str(" — ");
        suffix.push_str(artist);
    }
    if let Some(ms) = r.duration_ms {
        let _ = write!(suffix, " · {}", format_duration(ms));
    }

    truncate_smart(
        prefix,
        &r.title,
        if suffix.is_empty() {
            None
        } else {
            Some(&suffix)
        },
        100,
    )
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
    search_port: &Arc<dyn application::ports::MusicSearchPort>,
    youtube_repo: &Arc<dyn application::ports::youtube::YoutubeRepository>,
    youtube_search_repo: &Arc<dyn application::ports::repository::YoutubeSearchRepository>,
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

    if let Some(submission) = SubmissionValue::classify(asset_id_str) {
        match submission {
            SubmissionValue::TrackId(tid) => {
                resolved_tracks.push(tid);
            }
            SubmissionValue::YoutubeVideoId(video_id) => {
                let cache_result = youtube_search_repo
                    .find_search_cache_by_video_id(&video_id)
                    .await;

                if let Ok(Some(track)) = youtube_repo.find_track_by_video_id(&video_id).await {
                    resolved_tracks.push(track.id);
                } else if let Ok(Some(cache_row)) = cache_result {
                    let meta = domain::VideoMetadata {
                        video_id: cache_row.video_id.clone(),
                        title: Some(cache_row.title.clone()),
                        track_title: Some(cache_row.title),
                        uploader: cache_row.uploader.clone(),
                        artist: cache_row.uploader,
                        album: None,
                        url: format!("https://www.youtube.com/watch?v={}", cache_row.video_id),
                        channel_id: cache_row.channel_id,
                        duration_ms: cache_row.duration_ms.map(i64::from),
                        thumbnail_url: cache_row.thumbnail_url,
                    };

                    if let Ok(tid) = youtube_repo.create_youtube_stub(&meta).await {
                        resolved_tracks.push(tid);
                        let _ = youtube_search_repo
                            .link_search_cache_to_track(&video_id, tid)
                            .await;
                    }
                } else {
                    // Fresh URL paste — video not in tracks or search cache yet.
                    // Fetch metadata directly via yt-dlp and create a stub.
                    let canonical = adapters_ytdlp::canonical_youtube_url(&video_id);
                    match ytdlp_port.fetch_video_metadata(&canonical).await {
                        Ok(meta) => match youtube_repo.create_youtube_stub(&meta).await {
                            Ok(id) => {
                                resolved_tracks.push(id);
                                let _ = youtube_search_repo
                                    .link_search_cache_to_track(&video_id, id)
                                    .await;
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to create YouTube stub from vid submission");
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
                            tracing::error!(error = %e, video_id = %video_id, "Failed to fetch YouTube metadata for vid submission");
                            let _ = interaction
                                .edit_response(
                                    http,
                                    EditInteractionResponse::new()
                                        .content("Search result expired or no longer available."),
                                )
                                .await;
                            return;
                        }
                    }
                }
            }
            SubmissionValue::YoutubeSearch(query) => {
                // Live YouTube search — user clicked "🔎 Search YouTube" sentinel.
                // Use ytsearch1: to find the top result and create a stub.
                let search_url = format!("ytsearch1:{query}");
                match ytdlp_port.fetch_video_metadata(&search_url).await {
                    Ok(meta) => match youtube_repo.create_youtube_stub(&meta).await {
                        Ok(id) => {
                            resolved_tracks.push(id);
                            let _ = youtube_search_repo
                                .upsert_search_result(&query, &meta)
                                .await;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to create stub from YouTube search");
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
                        tracing::error!(error = %e, query = %query, "Live YouTube search failed");
                        let _ = interaction
                            .edit_response(
                                http,
                                EditInteractionResponse::new()
                                    .content("YouTube search returned no results."),
                            )
                            .await;
                        return;
                    }
                }
            }
        }
    } else if asset_id_str.starts_with("http")
        && (asset_id_str.contains("youtube.com") || asset_id_str.contains("youtu.be"))
    {
        tracing::info!(url = %asset_id_str, "Detected YouTube URL in play command");

        // Check if it's a playlist
        if let Some(_playlist_id) = adapters_ytdlp::extract_youtube_playlist_id(asset_id_str) {
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
                            EditInteractionResponse::new()
                                .content("Failed to fetch playlist information from YouTube."),
                        )
                        .await;
                    return;
                }
            }
        } else if let Some(video_id) = adapters_ytdlp::extract_youtube_video_id(asset_id_str) {
            let canonical = adapters_ytdlp::canonical_youtube_url(&video_id);
            match ytdlp_port.fetch_video_metadata(&canonical).await {
                Ok(meta) => match youtube_repo.create_youtube_stub(&meta).await {
                    Ok(id) => {
                        resolved_tracks.push(id);
                        // M7: Link newly created track to youtube_search_cache so searches aren't orphaned
                        let _ = youtube_search_repo
                            .link_search_cache_to_track(&canonical, id)
                            .await;
                    }
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
        match search_port
            .autocomplete(asset_id_str, SearchFilter::All, 1)
            .await
        {
            Ok(results) if !results.is_empty() => {
                let res = &results[0];
                if let Some(tid) = res.track_id {
                    resolved_tracks.push(tid);
                } else if let Some(vid) = &res.youtube_video_id {
                    // Turn stub into real via ytsearch cache lookup
                    let cache_result = youtube_search_repo.find_search_cache_by_video_id(vid).await;

                    if let Ok(Some(track)) = youtube_repo.find_track_by_video_id(vid).await {
                        resolved_tracks.push(track.id);
                    } else if let Ok(Some(cache_row)) = cache_result {
                        let meta = domain::VideoMetadata {
                            video_id: cache_row.video_id.clone(),
                            title: Some(cache_row.title.clone()),
                            track_title: Some(cache_row.title),
                            uploader: cache_row.uploader.clone(),
                            artist: cache_row.uploader,
                            album: None,
                            url: format!("https://www.youtube.com/watch?v={}", cache_row.video_id),
                            channel_id: cache_row.channel_id,
                            duration_ms: cache_row.duration_ms.map(i64::from),
                            thumbnail_url: cache_row.thumbnail_url,
                        };

                        if let Ok(tid) = youtube_repo.create_youtube_stub(&meta).await {
                            resolved_tracks.push(tid);
                            let _ = youtube_search_repo
                                .link_search_cache_to_track(vid, tid)
                                .await;
                        }
                    }
                }
            }
            _ => {
                // No local/indexed match — fall back to live YouTube search.
                let search_url = format!("ytsearch1:{asset_id_str}");
                match ytdlp_port.fetch_video_metadata(&search_url).await {
                    Ok(meta) => match youtube_repo.create_youtube_stub(&meta).await {
                        Ok(id) => {
                            resolved_tracks.push(id);
                            let _ = youtube_search_repo
                                .upsert_search_result(asset_id_str, &meta)
                                .await;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to create stub from YouTube search fallback");
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
                        tracing::warn!(error = %e, query = %asset_id_str, "YouTube search fallback failed");
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
    let mut last_artist = String::new();

    for track_id in resolved_tracks {
        // ── 3. Look up track metadata from the database ────────────────────────
        let Ok(Some(track)) = track_repo.find_by_id(track_id).await else {
            continue;
        };

        if added_tracks == 0 {
            last_title = track.title.clone();
            last_artist = track.artist_display.clone().unwrap_or_default();
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

    match added_tracks.cmp(&1) {
        std::cmp::Ordering::Equal => {
            let msg = if last_artist.is_empty() {
                format!("🎵 Queued: **{last_title}**")
            } else {
                format!("🎵 Queued: **{last_title}** — {last_artist}")
            };
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
        }
        std::cmp::Ordering::Greater => {
            let msg = format!("🎵 Queued **{added_tracks} tracks**");
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
        }
        std::cmp::Ordering::Less => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Failed to queue any tracks."),
                )
                .await;
        }
    }
}
