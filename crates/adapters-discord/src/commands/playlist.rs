use std::sync::Arc;

use serenity::all::Http;
use serenity::builder::{
    CreateAutocompleteResponse, CreateCommand, CreateCommandOption, CreateEmbed,
    CreateInteractionResponse, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, CommandOptionType, ResolvedValue};
use serenity::model::id::UserId;

use crate::commands::pagination::{PAGE_SIZE, build_nav_buttons, total_pages};
use adapters_voice::lifecycle::TrackLifecycleTx;
use adapters_voice::player::{enqueue_track, join_channel};
use adapters_voice::state::QueuedTrack;
use adapters_voice::state_map::GuildStateMap;
use adapters_voice::track_event_handler::post_now_playing;
use application::ports::playlist::PlaylistPort;
use application::ports::search::TrackSearchPort;
use serenity::all::Cache;
use serenity::model::id::ChannelId;
use std::path::Path;
use uuid::Uuid;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("playlist")
        .description("Manage your playlists")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "create",
                "Create a new playlist",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Playlist name")
                    .required(true),
            )
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "description",
                    "Playlist description",
                )
                .required(false),
            ),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommand, "delete", "Delete a playlist")
                .add_sub_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "name",
                        "Playlist to delete",
                    )
                    .required(true)
                    .set_autocomplete(true),
                ),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommand, "rename", "Rename a playlist")
                .add_sub_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "name",
                        "Current playlist name",
                    )
                    .required(true)
                    .set_autocomplete(true),
                )
                .add_sub_option(
                    CreateCommandOption::new(CommandOptionType::String, "new_name", "New name")
                        .required(true),
                ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "add",
                "Add a track to a playlist",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "playlist", "Playlist")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "track", "Track to add")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "remove",
                "Remove a track from a playlist",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "playlist", "Playlist")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "track", "Track to remove")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "play",
                "Play all tracks in a playlist",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Playlist to play")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommand, "list", "List your playlists")
                .add_sub_option(
                    CreateCommandOption::new(
                        CommandOptionType::User,
                        "user",
                        "Show another user's public playlists",
                    )
                    .required(false),
                ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "view",
                "View tracks in a playlist",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Playlist to view")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "share",
                "Toggle playlist public/private",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Playlist to share")
                    .required(true)
                    .set_autocomplete(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "invite",
                "Invite a collaborator",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Playlist")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::User, "user", "User to invite")
                    .required(true),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "kick",
                "Remove a collaborator",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Playlist")
                    .required(true)
                    .set_autocomplete(true),
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::User, "user", "User to remove")
                    .required(true),
            ),
        )
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    http: &Arc<Http>,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    _search_port: &Arc<dyn TrackSearchPort>,
) {
    let options = interaction.data.options();
    let subcmd = match options.first() {
        Some(opt) => opt,
        None => return,
    };

    let user_id = interaction.user.id.to_string();

    match subcmd.name {
        "create" => run_create(http, interaction, playlist_port, &user_id, subcmd).await,
        "delete" => run_delete(http, interaction, playlist_port, &user_id, subcmd).await,
        "rename" => run_rename(http, interaction, playlist_port, &user_id, subcmd).await,
        "add" => run_add(http, interaction, playlist_port, &user_id, subcmd).await,
        "remove" => run_remove(http, interaction, playlist_port, &user_id, subcmd).await,
        "list" => run_list(http, interaction, playlist_port, &user_id, subcmd).await,
        "view" => run_view(http, interaction, playlist_port, &user_id, subcmd).await,
        "share" => run_share(http, interaction, playlist_port, &user_id, subcmd).await,
        "invite" => run_invite(http, interaction, playlist_port, &user_id, subcmd).await,
        "kick" => run_kick(http, interaction, playlist_port, &user_id, subcmd).await,
        "play" => {
            // Handled exclusively in handler.rs to accommodate voice connections.
        }
        _ => {}
    }
}

pub async fn autocomplete(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    search_port: &Arc<dyn TrackSearchPort>,
) {
    use serenity::model::application::CommandDataOptionValue;

    let user_id = interaction.user.id.to_string();
    let subcmd = match interaction.data.options.first() {
        Some(opt) => opt,
        None => return,
    };

    let subcmd_name = subcmd.name.as_str();

    let sub_options = match &subcmd.value {
        CommandDataOptionValue::SubCommand(opts) => opts,
        _ => return,
    };

    // Find the focused option by looking for Autocomplete variant
    let focused = sub_options
        .iter()
        .find(|o| matches!(&o.value, CommandDataOptionValue::Autocomplete { .. }));
    let focused_opt = match focused {
        Some(o) => o,
        None => return,
    };

    let focused_value = focused_opt.value.as_str().unwrap_or("");

    match (subcmd_name, focused_opt.name.as_str()) {
        // Playlist name autocomplete — own playlists for write ops
        ("delete" | "rename" | "share" | "invite" | "kick", "name") => {
            autocomplete_own_playlists(http, interaction, playlist_port, &user_id, focused_value)
                .await;
        }
        // Playlist name autocomplete — accessible playlists for read ops
        ("play" | "view", "name") => {
            autocomplete_accessible_playlists(
                http,
                interaction,
                playlist_port,
                &user_id,
                focused_value,
            )
            .await;
        }
        // Playlist autocomplete for add/remove
        ("add" | "remove", "playlist") => {
            autocomplete_own_playlists(http, interaction, playlist_port, &user_id, focused_value)
                .await;
        }
        // Track search autocomplete for add
        ("add", "track") => {
            autocomplete_tracks(http, interaction, search_port, focused_value).await;
        }
        // Track autocomplete for remove — items in the selected playlist
        ("remove", "track") => {
            let playlist_id_str = sub_options
                .iter()
                .find(|o| o.name == "playlist")
                .and_then(|o| o.value.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok());
            if let Some(playlist_id) = playlist_id_str {
                autocomplete_playlist_items(
                    http,
                    interaction,
                    playlist_port,
                    &user_id,
                    playlist_id,
                )
                .await;
            }
        }
        _ => {}
    }
}

async fn autocomplete_own_playlists(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    query: &str,
) {
    let playlists = playlist_port
        .list_user_playlists(user_id)
        .await
        .unwrap_or_default();

    let query_lower = query.to_lowercase();
    let choices: Vec<_> = playlists
        .into_iter()
        .filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query_lower))
        .take(25)
        .map(|p| {
            let display = format!("{} ({} tracks)", p.name, p.track_count);
            serenity::builder::AutocompleteChoice::new(display, p.id.to_string())
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

async fn autocomplete_accessible_playlists(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    query: &str,
) {
    let playlists = playlist_port
        .list_accessible_playlists(user_id)
        .await
        .unwrap_or_default();

    let query_lower = query.to_lowercase();
    let choices: Vec<_> = playlists
        .into_iter()
        .filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query_lower))
        .take(25)
        .map(|p| {
            let display = if p.owner_id == user_id {
                format!("{} ({} tracks)", p.name, p.track_count)
            } else {
                format!("{} ({} tracks) [shared]", p.name, p.track_count)
            };
            serenity::builder::AutocompleteChoice::new(display, p.id.to_string())
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

async fn autocomplete_tracks(
    http: &Http,
    interaction: &CommandInteraction,
    search_port: &Arc<dyn TrackSearchPort>,
    query: &str,
) {
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
            tracing::warn!(error = %e, "Playlist track autocomplete failed");
        }
    }
}

async fn autocomplete_playlist_items(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    playlist_id: uuid::Uuid,
) {
    match playlist_port
        .get_playlist_items(playlist_id, user_id, 0, 25)
        .await
    {
        Ok(page) => {
            let choices: Vec<_> = page
                .items
                .into_iter()
                .map(|(item, track)| {
                    let display = format!("#{} — {}", item.position + 1, track.title);
                    serenity::builder::AutocompleteChoice::new(display, item.id.to_string())
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
            tracing::warn!(error = %e, "Playlist items autocomplete failed");
        }
    }
}

// ── Subcommand implementations ─────────────────────────────────────────

async fn run_create(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let opts = match &subcmd.value {
        ResolvedValue::SubCommand(opts) => opts,
        _ => return,
    };

    let name = opts
        .iter()
        .find(|o| o.name == "name")
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => Some(s),
            _ => None,
        })
        .unwrap_or("Untitled");

    let description = opts
        .iter()
        .find(|o| o.name == "description")
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => Some(s),
            _ => None,
        });

    match playlist_port
        .create_playlist(user_id, name, description)
        .await
    {
        Ok(playlist) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content(format!("✅ Created playlist **{}**", playlist.name)),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_delete(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "name");
    let Some(playlist_id) = playlist_id else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a playlist from the list."),
            )
            .await;
        return;
    };

    match playlist_port.delete_playlist(playlist_id, user_id).await {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("✅ Playlist deleted."),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_rename(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "name");
    let new_name = extract_string_option(subcmd, "new_name").unwrap_or("Untitled");

    let Some(playlist_id) = playlist_id else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a playlist from the list."),
            )
            .await;
        return;
    };

    match playlist_port
        .rename_playlist(playlist_id, user_id, new_name)
        .await
    {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new()
                        .content(format!("✅ Playlist renamed to **{new_name}**")),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_add(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "playlist");
    let track_id = extract_uuid_option(subcmd, "track");

    let (Some(playlist_id), Some(track_id)) = (playlist_id, track_id) else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("Please select both a playlist and a track from the lists."),
            )
            .await;
        return;
    };

    match playlist_port
        .add_track(playlist_id, track_id, user_id)
        .await
    {
        Ok(item) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(format!(
                        "✅ Added track at position **#{}**",
                        item.position + 1
                    )),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_remove(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "playlist");
    let item_id = extract_uuid_option(subcmd, "track");

    let (Some(playlist_id), Some(item_id)) = (playlist_id, item_id) else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("Please select both a playlist and a track from the lists."),
            )
            .await;
        return;
    };

    match playlist_port
        .remove_track(playlist_id, item_id, user_id)
        .await
    {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("✅ Track removed from playlist."),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_list(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let target_user = extract_user_option(subcmd, "user")
        .map(|u| u.to_string())
        .unwrap_or_else(|| user_id.to_string());

    let is_self = target_user == user_id;

    let playlists = if is_self {
        playlist_port.list_user_playlists(user_id).await
    } else {
        // Show only public playlists for other users
        playlist_port
            .list_accessible_playlists(user_id)
            .await
            .map(|mut pl| {
                pl.retain(|p| p.owner_id == target_user);
                pl
            })
    };

    match playlists {
        Ok(playlists) if playlists.is_empty() => {
            let msg = if is_self {
                "You don't have any playlists yet. Create one with `/playlist create`."
            } else {
                "This user has no public playlists."
            };
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
        }
        Ok(playlists) => {
            let mut description = String::new();
            for p in &playlists {
                let visibility = if p.visibility == domain::PlaylistVisibility::Public {
                    " 🌐"
                } else {
                    " 🔒"
                };
                description.push_str(&format!(
                    "**{}**{} — {} track{}\n",
                    p.name,
                    visibility,
                    p.track_count,
                    if p.track_count == 1 { "" } else { "s" }
                ));
            }

            let title = if is_self {
                "Your Playlists".to_string()
            } else {
                format!("<@{target_user}>'s Public Playlists")
            };

            let embed = CreateEmbed::new()
                .title(title)
                .description(description)
                .color(0x5865F2);

            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().embed(embed))
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_view(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer(http).await;
    let playlist_id = extract_uuid_option(subcmd, "name");
    let Some(playlist_id) = playlist_id else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a playlist from the list."),
            )
            .await;
        return;
    };

    match playlist_port
        .get_playlist_items(playlist_id, user_id, 0, PAGE_SIZE)
        .await
    {
        Ok(page) => {
            let pages = total_pages(page.total, PAGE_SIZE);
            let embed = build_playlist_embed(&page.items, playlist_id, 0, pages, page.total);
            let buttons =
                build_nav_buttons("playlist_page", &playlist_id.to_string(), 0, pages, user_id);

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
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_share(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "name");
    let Some(playlist_id) = playlist_id else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a playlist from the list."),
            )
            .await;
        return;
    };

    // Toggle: check current visibility first
    let playlists = playlist_port
        .list_user_playlists(user_id)
        .await
        .unwrap_or_default();

    let current = playlists.iter().find(|p| p.id == playlist_id);
    let new_visibility = match current {
        Some(p) if p.visibility == domain::PlaylistVisibility::Public => {
            domain::PlaylistVisibility::Private
        }
        _ => domain::PlaylistVisibility::Public,
    };

    match playlist_port
        .set_visibility(playlist_id, user_id, new_visibility.clone())
        .await
    {
        Ok(()) => {
            let msg = match new_visibility {
                domain::PlaylistVisibility::Public => "🌐 Playlist is now **public**.",
                domain::PlaylistVisibility::Private => "🔒 Playlist is now **private**.",
            };
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_invite(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "name");
    let target = extract_user_option(subcmd, "user");

    let (Some(playlist_id), Some(target_id)) = (playlist_id, target) else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a playlist and a user."),
            )
            .await;
        return;
    };

    match playlist_port
        .add_collaborator(playlist_id, user_id, &target_id.to_string())
        .await
    {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(format!(
                        "✅ <@{target_id}> can now add tracks to your playlist."
                    )),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

async fn run_kick(
    http: &Http,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    user_id: &str,
    subcmd: &serenity::model::application::ResolvedOption<'_>,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let playlist_id = extract_uuid_option(subcmd, "name");
    let target = extract_user_option(subcmd, "user");

    let (Some(playlist_id), Some(target_id)) = (playlist_id, target) else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Please select a playlist and a user."),
            )
            .await;
        return;
    };

    match playlist_port
        .remove_collaborator(playlist_id, user_id, &target_id.to_string())
        .await
    {
        Ok(()) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(format!(
                        "✅ <@{target_id}> removed as collaborator. Their tracks remain."
                    )),
                )
                .await;
        }
        Err(e) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(error_to_user_message(&e)),
                )
                .await;
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn extract_uuid_option(
    subcmd: &serenity::model::application::ResolvedOption<'_>,
    name: &str,
) -> Option<uuid::Uuid> {
    let opts = match &subcmd.value {
        ResolvedValue::SubCommand(opts) => opts,
        _ => return None,
    };
    opts.iter()
        .find(|o| o.name == name)
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => uuid::Uuid::parse_str(s).ok(),
            _ => None,
        })
}

fn extract_string_option<'a>(
    subcmd: &'a serenity::model::application::ResolvedOption<'a>,
    name: &str,
) -> Option<&'a str> {
    let opts = match &subcmd.value {
        ResolvedValue::SubCommand(opts) => opts,
        _ => return None,
    };
    opts.iter()
        .find(|o| o.name == name)
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => Some(s),
            _ => None,
        })
}

fn extract_user_option(
    subcmd: &serenity::model::application::ResolvedOption<'_>,
    name: &str,
) -> Option<UserId> {
    let opts = match &subcmd.value {
        ResolvedValue::SubCommand(opts) => opts,
        _ => return None,
    };
    opts.iter()
        .find(|o| o.name == name)
        .and_then(|o| match o.value {
            ResolvedValue::User(user, _) => Some(user.id),
            _ => None,
        })
}

pub fn build_playlist_embed<'a>(
    items: &[(domain::PlaylistItem, domain::track::TrackSummary)],
    _playlist_id: uuid::Uuid,
    page: i64,
    total_pages: i64,
    total_tracks: i64,
) -> CreateEmbed<'a> {
    let mut description = String::new();
    if items.is_empty() {
        description.push_str("*No tracks yet — add some with `/playlist add`*");
    } else {
        for (i, (_item, track)) in items.iter().enumerate() {
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
            "Playlist — {total_tracks} track{}",
            if total_tracks == 1 { "" } else { "s" }
        ))
        .description(description)
        .color(0x5865F2)
        .footer(serenity::builder::CreateEmbedFooter::new(format!(
            "Page {}/{}",
            page + 1,
            total_pages.max(1)
        )))
}

fn error_to_user_message(err: &application::AppError) -> String {
    use application::error::PlaylistErrorKind;
    match err {
        application::AppError::Playlist { kind, .. } => match kind {
            PlaylistErrorKind::NotFound => "Playlist not found.".to_string(),
            PlaylistErrorKind::Forbidden => "You don't have permission to do that.".to_string(),
            PlaylistErrorKind::AlreadyExists => {
                "You already have a playlist with that name.".to_string()
            }
            PlaylistErrorKind::CollaboratorLimit => {
                "This playlist has reached the collaborator limit.".to_string()
            }
        },
        _ => {
            tracing::warn!(error = %err, "unexpected error in playlist command");
            "Something went wrong. Please try again.".to_string()
        }
    }
}

pub async fn run_play(
    http: &Arc<Http>,
    cache: &Arc<Cache>,
    interaction: &CommandInteraction,
    playlist_port: &Arc<dyn PlaylistPort>,
    media_root: &Path,
    auto_leave_secs: u64,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
    lifecycle_tx: &TrackLifecycleTx,
) {
    let _ = interaction.defer(http).await;

    let subcmd = match interaction.data.options.first() {
        Some(opt) => opt,
        None => return,
    };

    let playlist_id_str = match subcmd.value {
        serenity::model::application::CommandDataOptionValue::SubCommand(ref opts) => {
            match opts.first() {
                Some(opt) if opt.name == "name" => opt.value.as_str().unwrap_or(""),
                _ => "",
            }
        }
        _ => "",
    };

    let playlist_id = match Uuid::parse_str(playlist_id_str) {
        Ok(id) => id,
        Err(_) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Invalid playlist selected."),
                )
                .await;
            return;
        }
    };

    let user_id_str = interaction.user.id.to_string();

    let tracks = match playlist_port
        .get_playlist_tracks(playlist_id, &user_id_str)
        .await
    {
        Ok(t) => t,
        Err(_) => {
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content("Playlist not found or access denied."),
                )
                .await;
            return;
        }
    };

    if tracks.is_empty() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Playlist is empty."),
            )
            .await;
        return;
    }

    let guild_id = interaction.guild_id.unwrap_or_default();

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

    let existing_channel = guild_state_map
        .get(&guild_id)
        .and_then(|s| s.try_lock().ok().and_then(|state| state.voice_channel_id));

    if let Some(existing) = existing_channel
        && existing != channel_id
    {
        if let Some(handler_lock) = songbird.get(guild_id) {
            handler_lock.lock().await.queue().stop();
        }
        if let Some(state_lock) = guild_state_map.get(&guild_id) {
            let mut state = state_lock.lock().await;
            state.meta_queue.clear();
            state.cancel_auto_leave();
        }
    }

    if join_channel(songbird, guild_id, channel_id).await.is_err() {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("Failed to join your voice channel."),
            )
            .await;
        return;
    }

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
    }

    let mut enqueued = 0;

    for summary in tracks {
        let queued = QueuedTrack::from(summary);

        {
            let mut state = state_lock.lock().await;
            state.meta_queue.push_back(queued.clone());
        }

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
                enqueued += 1;
            }
            Err(e) => {
                {
                    let mut state = state_lock.lock().await;
                    state.meta_queue.pop_back();
                }
                tracing::warn!(guild_id = %guild_id, track_id = %queued.track_id, error = %e, "skipped missing track during playlist playback");
            }
        }
    }

    if enqueued == 0 {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new()
                    .content("All tracks in the playlist were missing or invalid."),
            )
            .await;
    } else {
        let msg = format!("✅ Enqueued **{}** tracks from the playlist.", enqueued);
        let _ = interaction
            .edit_response(http, EditInteractionResponse::new().content(msg))
            .await;

        let should_post = {
            let state = state_lock.lock().await;
            state.meta_queue.len() == enqueued // If queue length matches enqueued, we are playing first track
        };

        if should_post {
            let text_channel = ChannelId::new(interaction.channel_id.get());
            let first_track = {
                let state = state_lock.lock().await;
                state.meta_queue.front().cloned()
            };
            if let Some(track) = first_track {
                post_now_playing(
                    http,
                    text_channel,
                    guild_id,
                    &state_lock,
                    Some(&track),
                    None,
                )
                .await;
            }
        }
    }
}
