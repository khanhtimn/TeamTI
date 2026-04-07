use std::sync::Arc;

use serenity::all::Http;
use serenity::builder::{
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, CreateEmbed,
    CreateInteractionResponse, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, CommandOptionType, ResolvedValue};
use uuid::Uuid;

use crate::commands::pagination::{PAGE_SIZE, build_nav_buttons, total_pages};
use adapters_voice::state_map::GuildStateMap;
use application::ports::user_library::UserLibraryPort;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("favourite")
        .description("Manage your favourite tracks")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "add",
                "Favourite a track (defaults to currently playing)",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "track", "Track to favourite")
                    .required(false)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "remove",
                "Remove a track from favourites",
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "track",
                    "Track to unfavourite",
                )
                .required(false)
                .set_autocomplete(true),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "list",
            "View your favourite tracks",
        ))
}

pub async fn run(
    http: &Arc<Http>,
    interaction: &CommandInteraction,
    user_library_port: &Arc<dyn UserLibraryPort>,
    guild_state: &Arc<GuildStateMap>,
) {
    let options = interaction.data.options();
    let subcmd = match options.first() {
        Some(opt) => opt,
        None => return,
    };

    let user_id = interaction.user.id.to_string();

    match subcmd.name {
        "add" => {
            run_add(
                http,
                interaction,
                user_library_port,
                guild_state,
                &user_id,
                subcmd,
            )
            .await;
        }
        "remove" => {
            run_remove(
                http,
                interaction,
                user_library_port,
                guild_state,
                &user_id,
                subcmd,
            )
            .await;
        }
        "list" => run_list(http, interaction, user_library_port, &user_id).await,
        _ => {}
    }
}

pub async fn autocomplete(
    http: &Http,
    interaction: &CommandInteraction,
    search_port: &Arc<dyn application::ports::search::TrackSearchPort>,
) {
    // Track autocomplete for add/remove: get the focused value from raw options
    let subcmd = match interaction.data.options.first() {
        Some(opt) => opt,
        None => return,
    };

    use serenity::model::application::CommandDataOptionValue;

    let sub_options = match &subcmd.value {
        CommandDataOptionValue::SubCommand(opts) => opts,
        _ => return,
    };

    // Find the focused autocomplete option
    let query = sub_options
        .iter()
        .find_map(|o| {
            if o.name == "track" {
                o.value.as_str()
            } else {
                None
            }
        })
        .unwrap_or("");

    match search_port.autocomplete(query, 25).await {
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
                    CreateInteractionResponse::Autocomplete(
                        CreateAutocompleteResponse::new().set_choices(choices),
                    ),
                )
                .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Favourite autocomplete failed");
        }
    }
}

async fn run_add(
    http: &Http,
    interaction: &CommandInteraction,
    user_library_port: &Arc<dyn UserLibraryPort>,
    guild_state: &Arc<GuildStateMap>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    // Try to get track_id from option, otherwise use currently playing
    let track_id = extract_uuid_option(subcmd, "track").or_else(|| {
        let guild_id = interaction.guild_id?;
        let state_lock = guild_state.get(&guild_id)?;
        let state = state_lock.try_lock().ok()?;
        state.meta_queue.front().map(|t| t.track_id)
    });

    let Some(track_id) = track_id else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("No track specified and nothing is currently playing."),
            )
            .await;
        return;
    };

    match user_library_port.add_favourite(user_id, track_id).await {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("❤️ Added to favourites."),
                )
                .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to add favourite");
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content("Something went wrong. Please try again."),
                )
                .await;
        }
    }
}

async fn run_remove(
    http: &Http,
    interaction: &CommandInteraction,
    user_library_port: &Arc<dyn UserLibraryPort>,
    guild_state: &Arc<GuildStateMap>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let track_id = extract_uuid_option(subcmd, "track").or_else(|| {
        let guild_id = interaction.guild_id?;
        let state_lock = guild_state.get(&guild_id)?;
        let state = state_lock.try_lock().ok()?;
        state.meta_queue.front().map(|t| t.track_id)
    });

    let Some(track_id) = track_id else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("No track specified and nothing is currently playing."),
            )
            .await;
        return;
    };

    match user_library_port.remove_favourite(user_id, track_id).await {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("💔 Removed from favourites."),
                )
                .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to remove favourite");
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content("Something went wrong. Please try again."),
                )
                .await;
        }
    }
}

async fn run_list(
    http: &Http,
    interaction: &CommandInteraction,
    user_library_port: &Arc<dyn UserLibraryPort>,
    user_id: &str,
) {
    let _ = interaction.defer_ephemeral(http).await;

    match user_library_port
        .list_favourites(user_id, 0, PAGE_SIZE)
        .await
    {
        Ok(page) => {
            let pages = total_pages(page.total, PAGE_SIZE);
            let embed = build_favourites_embed(&page.tracks, 0, pages, page.total);
            let buttons = build_nav_buttons("fav_page", user_id, 0, pages, user_id);

            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .embed(embed)
                        .components(vec![buttons]),
                )
                .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list favourites");
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content("Something went wrong. Please try again."),
                )
                .await;
        }
    }
}

pub fn build_favourites_embed<'a>(
    tracks: &[domain::track::TrackSummary],
    page: i64,
    pages: i64,
    total: i64,
) -> CreateEmbed<'a> {
    let mut description = String::new();
    if tracks.is_empty() {
        description.push_str("*No favourites yet — use `/favourite add` to start.*");
    } else {
        for (i, track) in tracks.iter().enumerate() {
            let idx = page * PAGE_SIZE + i as i64 + 1;
            let artist = track.artist_display.as_deref().unwrap_or("Unknown Artist");
            let dur = track
                .duration_ms
                .map(|ms| {
                    let s = ms / 1000;
                    format!("{}:{:02}", s / 60, s % 60)
                })
                .unwrap_or_default();
            description.push_str(&format!(
                "`{idx}.` **{}** — {artist} `{dur}`\n",
                track.title
            ));
        }
    }

    CreateEmbed::new()
        .title(format!(
            "❤️ Favourites — {total} track{}",
            if total == 1 { "" } else { "s" }
        ))
        .description(description)
        .color(0xE91E63)
        .footer(serenity::builder::CreateEmbedFooter::new(format!(
            "Page {}/{}",
            page + 1,
            pages.max(1)
        )))
}

fn extract_uuid_option(
    subcmd: &serenity::model::application::ResolvedOption<'_>,
    name: &str,
) -> Option<Uuid> {
    let opts = match &subcmd.value {
        ResolvedValue::SubCommand(opts) => opts,
        _ => return None,
    };
    opts.iter()
        .find(|o| o.name == name)
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => Uuid::parse_str(s).ok(),
            _ => None,
        })
}
