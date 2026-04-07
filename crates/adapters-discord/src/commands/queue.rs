use std::sync::Arc;

use serenity::all::{ButtonStyle, Cache, Http};
use serenity::builder::{
    CreateActionRow, CreateAutocompleteResponse, CreateButton, CreateCommand, CreateCommandOption,
    CreateComponent, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, EditInteractionResponse,
};
use serenity::model::application::{CommandInteraction, CommandOptionType, ComponentInteraction};
use serenity::model::id::GuildId;
use serenity::model::permissions::Permissions;

use adapters_voice::state::QueueSource;
use adapters_voice::state_map::GuildStateMap;
use adapters_voice::track_event_handler::format_duration_ms;
use application::ports::playlist::PlaylistPort;

use crate::ui::custom_id::QueueAction;

const PAGE_SIZE: usize = 10;

// ── Registration ────────────────────────────────────────────────────────

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("queue")
        .description("View the music queue")
        .default_member_permissions(Permissions::SEND_MESSAGES)
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "save_as",
                "Save the queue as a playlist with this name",
            )
            .required(false),
        )
}

// ── Autocomplete ────────────────────────────────────────────────────────

pub async fn autocomplete(
    http: &Http,
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
                .skip(1) // skip position 0 (currently playing)
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
            CreateInteractionResponse::Autocomplete(
                CreateAutocompleteResponse::new().set_choices(choices),
            ),
        )
        .await;
}

// ── Command Router ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run(
    http: &Arc<Http>,
    interaction: &CommandInteraction,
    _songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
    playlist_port: &Arc<dyn PlaylistPort>,
) {
    let save_as = interaction
        .data
        .options
        .iter()
        .find(|opt| opt.name == "save_as")
        .and_then(|opt| opt.value.as_str());

    if let Some(name) = save_as {
        run_save(http, interaction, guild_state_map, playlist_port, name).await;
    } else {
        run_view(http, interaction, guild_state_map).await;
    }
}
// ── /queue view ─────────────────────────────────────────────────────────

async fn run_view(
    http: &Http,
    interaction: &CommandInteraction,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let _ = interaction.defer(http).await;

    let guild_id = interaction.guild_id.unwrap_or_default();
    let user_id = interaction.user.id.to_string();

    let state_lock = if let Some(s) = guild_state_map.get(&guild_id) {
        s.clone()
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is empty."),
            )
            .await;
        return;
    };

    let state = state_lock.lock().await;
    if state.meta_queue.is_empty() {
        drop(state);
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is empty."),
            )
            .await;
        return;
    }

    let embed = build_queue_embed(&state, 0);
    let total_pages = queue_total_pages(state.meta_queue.len());
    let nav_row = build_queue_nav_buttons(guild_id, 0, total_pages, &user_id);
    let action_row = build_queue_action_buttons(guild_id, &state, &user_id);
    drop(state);

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new()
                .embed(embed)
                .components(vec![nav_row, action_row]),
        )
        .await;
}

// ── /queue save ─────────────────────────────────────────────────────────

async fn run_save(
    http: &Http,
    interaction: &CommandInteraction,
    guild_state_map: &Arc<GuildStateMap>,
    playlist_port: &Arc<dyn PlaylistPort>,
    provided_name: &str,
) {
    let _ = interaction.defer_ephemeral(http).await;
    let guild_id = interaction.guild_id.unwrap_or_default();
    let user_id = interaction.user.id.to_string();

    let name = provided_name.to_string();

    let state_lock = if let Some(s) = guild_state_map.get(&guild_id) {
        s.clone()
    } else {
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is empty."),
            )
            .await;
        return;
    };

    let state = state_lock.lock().await;
    if state.meta_queue.is_empty() {
        drop(state);
        let _ = interaction
            .edit_response(
                http,
                EditInteractionResponse::new().content("The queue is empty."),
            )
            .await;
        return;
    }

    let track_ids: Vec<_> = state.meta_queue.iter().map(|t| t.track_id).collect();
    let track_count = track_ids.len();
    drop(state);

    // Create playlist
    let playlist = match playlist_port.create_playlist(&user_id, &name, None).await {
        Ok(p) => p,
        Err(e) => {
            use application::error::PlaylistErrorKind;
            let msg = match &e {
                application::AppError::Playlist {
                    kind: PlaylistErrorKind::AlreadyExists,
                    ..
                } => format!(
                    "A playlist named \"{name}\" already exists. Try /queue save <different-name>."
                ),
                _ => "Failed to create playlist.".to_string(),
            };
            let _ = interaction
                .edit_response(http, EditInteractionResponse::new().content(msg))
                .await;
            return;
        }
    };

    // Add all tracks
    for track_id in &track_ids {
        if let Err(e) = playlist_port
            .add_track(playlist.id, *track_id, &user_id)
            .await
        {
            tracing::warn!(
                error = %e,
                track_id = %track_id,
                "failed to add track to saved queue playlist"
            );
        }
    }

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new().content(format!(
                "✅ Saved **{track_count} tracks** as playlist **\"{name}\"**."
            )),
        )
        .await;
}

// ── Queue Embed Builder ─────────────────────────────────────────────────

pub fn build_queue_embed<'a>(
    state: &adapters_voice::state::GuildMusicState,
    page: usize,
) -> CreateEmbed<'a> {
    let queue = &state.meta_queue;
    let total = queue.len();
    let total_pages = queue_total_pages(total);

    // Calculate remaining duration (positions 1..end)
    let remaining_ms: i64 = queue
        .iter()
        .skip(1)
        .map(|t| t.duration_ms.unwrap_or(0))
        .sum();
    let remaining_min = remaining_ms / 60_000;

    let title = format!(
        "📋  QUEUE  —  {} track{}  •  ~{} min remaining",
        total,
        if total == 1 { "" } else { "s" },
        remaining_min,
    );

    let start = page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(total);

    let mut description = String::new();
    let page_len = end - start;
    for (i, track) in queue.iter().enumerate().skip(start).take(page_len) {
        let marker = if i == 0 { "▶" } else { &format!("`{i}.`") };
        let radio_prefix = if track.source == QueueSource::Radio {
            "🎲 "
        } else {
            ""
        };
        let dur = track
            .duration_ms
            .map_or_else(|| "--:--".to_string(), |ms| format_duration_ms(ms.max(0)));
        let added_by = if track.source == QueueSource::Radio {
            Some("🎲 radio".to_string())
        } else if track.added_by.is_empty() {
            None
        } else {
            Some(format!("<@{}>", track.added_by))
        };

        let _ = core::fmt::write(
            &mut description,
            format_args!(
                "{marker}  {radio_prefix}**{}**\n    {}  •  {dur}",
                track.title, track.artist,
            ),
        );

        if let Some(added_by) = added_by {
            let _ = core::fmt::write(&mut description, format_args!("  •  {added_by}"));
        }
        description.push('\n');
        if i < end - 1 {
            description.push('\n');
        }
    }

    if description.is_empty() {
        description = "Queue is empty.".to_string();
    }

    CreateEmbed::new()
        .title(title)
        .description(description)
        .color(0x5865F2) // Discord blurple
        .footer(CreateEmbedFooter::new(format!(
            "Page {}/{}",
            page + 1,
            total_pages.max(1)
        )))
}

// ── Queue Button Builders ───────────────────────────────────────────────

fn build_queue_nav_buttons<'a>(
    guild_id: GuildId,
    page: usize,
    total_pages: usize,
    user_id: &str,
) -> CreateComponent<'a> {
    let prev_disabled = page == 0;
    let next_disabled = page >= total_pages.saturating_sub(1);

    let prev_id = QueueAction::PrevPage {
        guild_id,
        page: page.saturating_sub(1),
        user_id: user_id.to_string(),
    };
    let prev = CreateButton::new(prev_id.to_custom_id())
        .label("◀ Prev")
        .style(ButtonStyle::Secondary)
        .disabled(prev_disabled);

    let indicator = CreateButton::new(format!("qi|{guild_id}"))
        .label(format!("Page {}/{}", page + 1, total_pages.max(1)))
        .style(ButtonStyle::Secondary)
        .disabled(true);

    let next_id = QueueAction::NextPage {
        guild_id,
        page: page + 1,
        user_id: user_id.to_string(),
    };
    let next = CreateButton::new(next_id.to_custom_id())
        .label("Next ▶")
        .style(ButtonStyle::Secondary)
        .disabled(next_disabled);

    CreateComponent::ActionRow(CreateActionRow::Buttons(vec![prev, indicator, next].into()))
}

fn build_queue_action_buttons<'a>(
    guild_id: GuildId,
    state: &adapters_voice::state::GuildMusicState,
    user_id: &str,
) -> CreateComponent<'a> {
    let is_paused = state.is_paused();
    let queue_len = state.meta_queue.len();
    let uid = user_id.to_string();

    let pause_resume = CreateButton::new(
        QueueAction::Pause {
            guild_id,
            user_id: uid.clone(),
        }
        .to_custom_id(),
    )
    .label(if is_paused { "▶ Resume" } else { "⏸ Pause" })
    .style(if is_paused {
        ButtonStyle::Success
    } else {
        ButtonStyle::Secondary
    });

    let skip = CreateButton::new(
        QueueAction::Skip {
            guild_id,
            user_id: uid.clone(),
        }
        .to_custom_id(),
    )
    .label("⏭ Skip")
    .style(ButtonStyle::Secondary)
    .disabled(queue_len <= 1);

    let shuffle = CreateButton::new(
        QueueAction::Shuffle {
            guild_id,
            user_id: uid.clone(),
        }
        .to_custom_id(),
    )
    .label("🔀 Shuffle")
    .style(ButtonStyle::Secondary)
    .disabled(queue_len <= 2);

    let clear = CreateButton::new(
        QueueAction::Clear {
            guild_id,
            user_id: uid,
        }
        .to_custom_id(),
    )
    .label("🗑️ Clear")
    .style(ButtonStyle::Danger)
    .disabled(queue_len <= 1);

    CreateComponent::ActionRow(CreateActionRow::Buttons(
        vec![pause_resume, skip, shuffle, clear].into(),
    ))
}

fn queue_total_pages(total: usize) -> usize {
    if total == 0 {
        1
    } else {
        total.div_ceil(PAGE_SIZE)
    }
}

// ── Button Interaction Handlers ─────────────────────────────────────────

/// Handle queue action button presses. Called from handler.rs.
pub async fn handle_queue_button(
    http: &Http,
    cache: &Arc<Cache>,
    interaction: &ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let Some(action) = QueueAction::from_custom_id(&interaction.data.custom_id) else {
        return;
    };

    match action {
        QueueAction::PrevPage { page, user_id, .. }
        | QueueAction::NextPage { page, user_id, .. } => {
            handle_queue_pagination(http, interaction, guild_state_map, page, &user_id).await;
        }
        QueueAction::Pause { .. } => {
            handle_queue_pause(http, interaction, songbird, guild_state_map).await;
        }
        QueueAction::Skip { .. } => {
            handle_queue_skip(http, cache, interaction, songbird, guild_state_map).await;
        }
        QueueAction::Shuffle { .. } => {
            handle_queue_shuffle(http, interaction, songbird, guild_state_map).await;
        }
        QueueAction::Clear { .. } => {
            handle_queue_clear(http, interaction, songbird, guild_state_map).await;
        }
    }
}

async fn handle_queue_pagination(
    http: &Http,
    interaction: &ComponentInteraction,
    guild_state_map: &Arc<GuildStateMap>,
    page: usize,
    session_user_id: &str,
) {
    // Prev/Next are user-gated
    if interaction.user.id.to_string() != session_user_id {
        let resp = CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("This isn't your session — run /queue yourself.")
                .ephemeral(true),
        );
        let _ = interaction.create_response(http, resp).await;
        return;
    }

    let guild_id = interaction.guild_id.unwrap_or_default();
    let state_lock = match guild_state_map.get(&guild_id) {
        Some(s) => s.clone(),
        None => return,
    };

    let state = state_lock.lock().await;
    let embed = build_queue_embed(&state, page);
    let total_pages = queue_total_pages(state.meta_queue.len());
    let nav_row = build_queue_nav_buttons(guild_id, page, total_pages, session_user_id);
    let action_row = build_queue_action_buttons(guild_id, &state, session_user_id);
    drop(state);

    let resp = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![nav_row, action_row]),
    );
    let _ = interaction.create_response(http, resp).await;
}

async fn handle_queue_pause(
    http: &Http,
    interaction: &ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let guild_id = interaction.guild_id.unwrap_or_default();

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

    // Re-render queue embed
    let user_id = interaction.user.id.to_string();
    let state = state_lock.lock().await;
    let embed = build_queue_embed(&state, 0);
    let total_pages = queue_total_pages(state.meta_queue.len());
    let nav_row = build_queue_nav_buttons(guild_id, 0, total_pages, &user_id);
    let action_row = build_queue_action_buttons(guild_id, &state, &user_id);
    drop(state);

    let resp = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![nav_row, action_row]),
    );
    let _ = interaction.create_response(http, resp).await;
}

async fn handle_queue_skip(
    http: &Http,
    _cache: &Arc<Cache>,
    interaction: &ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = match guild_state_map.get(&guild_id) {
        Some(s) => s.clone(),
        None => return,
    };

    // Skip one track (same as /skip with no args)
    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        let _ = handler.queue().skip();
    }

    // Note: TrackEventHandler fires on skip and handles meta_queue pop,
    // NP update, and lifecycle events. We just re-render the queue.
    // Give a small delay for the event handler to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let user_id = interaction.user.id.to_string();
    let state = state_lock.lock().await;
    let embed = build_queue_embed(&state, 0);
    let total_pages = queue_total_pages(state.meta_queue.len());
    let nav_row = build_queue_nav_buttons(guild_id, 0, total_pages, &user_id);
    let action_row = build_queue_action_buttons(guild_id, &state, &user_id);
    drop(state);

    let resp = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![nav_row, action_row]),
    );
    let _ = interaction.create_response(http, resp).await;
}

async fn handle_queue_shuffle(
    http: &Http,
    interaction: &ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = match guild_state_map.get(&guild_id) {
        Some(s) => s.clone(),
        None => return,
    };

    let mut state = state_lock.lock().await;
    let original_len = state.meta_queue.len().saturating_sub(1);
    if original_len > 1 {
        let tail: Vec<_> = state.meta_queue.drain(1..).collect();

        // Build a single shuffled index permutation
        let mut indices: Vec<usize> = (0..original_len).collect();
        fastrand::shuffle(&mut indices);

        // Rebuild meta_queue tail in shuffled order
        let mut opts: Vec<_> = tail.into_iter().map(Some).collect();
        for &old_pos in &indices {
            if let Some(item) = opts[old_pos].take() {
                state.meta_queue.push_back(item);
            }
        }
        drop(state);

        // Apply the same permutation to Songbird's queue
        if let Some(handler_lock) = songbird.get(guild_id) {
            let handler = handler_lock.lock().await;
            handler.queue().modify_queue(|q| {
                if q.len() > 1 {
                    let mut sb_tail: Vec<_> = q.drain(1..).collect();
                    if sb_tail.len() == original_len {
                        let mut sb_opts: Vec<_> = sb_tail.drain(..).map(Some).collect();
                        for &old_pos in &indices {
                            if let Some(item) = sb_opts[old_pos].take() {
                                q.push_back(item);
                            }
                        }
                    } else {
                        for item in sb_tail {
                            q.push_back(item);
                        }
                    }
                }
            });
        }
    } else {
        drop(state);
    }

    // Re-render
    let user_id = interaction.user.id.to_string();
    let state = state_lock.lock().await;
    let embed = build_queue_embed(&state, 0);
    let total_pages = queue_total_pages(state.meta_queue.len());
    let nav_row = build_queue_nav_buttons(guild_id, 0, total_pages, &user_id);
    let action_row = build_queue_action_buttons(guild_id, &state, &user_id);
    drop(state);

    let resp = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![nav_row, action_row]),
    );
    let _ = interaction.create_response(http, resp).await;
}

async fn handle_queue_clear(
    http: &Http,
    interaction: &ComponentInteraction,
    songbird: &Arc<songbird::Songbird>,
    guild_state_map: &Arc<GuildStateMap>,
) {
    let guild_id = interaction.guild_id.unwrap_or_default();

    let state_lock = match guild_state_map.get(&guild_id) {
        Some(s) => s.clone(),
        None => return,
    };

    // Drain meta_queue first — source of truth
    {
        let mut state = state_lock.lock().await;
        state.meta_queue.truncate(1);
    }

    if let Some(handler_lock) = songbird.get(guild_id) {
        let handler = handler_lock.lock().await;
        handler.queue().modify_queue(|q| {
            q.drain(1..);
        });
    }

    // Re-render
    let user_id = interaction.user.id.to_string();
    let state = state_lock.lock().await;
    let embed = build_queue_embed(&state, 0);
    let total_pages = queue_total_pages(state.meta_queue.len());
    let nav_row = build_queue_nav_buttons(guild_id, 0, total_pages, &user_id);
    let action_row = build_queue_action_buttons(guild_id, &state, &user_id);
    drop(state);

    let resp = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![nav_row, action_row]),
    );
    let _ = interaction.create_response(http, resp).await;
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn format_queue_choice(index: usize, track: &adapters_voice::state::QueuedTrack) -> String {
    let raw = format!("{}. {} — {}", index, track.title, track.artist);
    if raw.len() > 100 {
        let end = raw.floor_char_boundary(97);
        format!("{}...", &raw[..end])
    } else {
        raw
    }
}
