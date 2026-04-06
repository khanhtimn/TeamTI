use std::sync::Arc;

use serenity::all::Http;
use serenity::builder::{CreateCommand, CreateEmbed, EditInteractionResponse};
use serenity::model::application::CommandInteraction;

use crate::commands::pagination::{PAGE_SIZE, build_nav_buttons, total_pages};
use application::ports::user_library::UserLibraryPort;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("history").description("View your recent listen history")
}

pub async fn run(
    http: &Arc<Http>,
    interaction: &CommandInteraction,
    user_library_port: &Arc<dyn UserLibraryPort>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    let user_id = interaction.user.id.to_string();

    // For history, we fetch a larger batch and paginate client-side
    // since recent_history returns distinct tracks.
    match user_library_port.recent_history(&user_id, 50).await {
        Ok(tracks) => {
            let total = tracks.len() as i64;
            let pages = total_pages(total, PAGE_SIZE);
            let page_tracks: Vec<_> = tracks.into_iter().take(PAGE_SIZE as usize).collect();

            let embed = build_history_embed(&page_tracks, 0, pages, total);
            let buttons = build_nav_buttons("history_page", &user_id, 0, pages, &user_id);

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
            tracing::warn!(error = %e, "Failed to fetch listen history");
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

pub fn build_history_embed<'a>(
    tracks: &[domain::track::TrackSummary],
    page: i64,
    pages: i64,
    total: i64,
) -> CreateEmbed<'a> {
    let mut description = String::new();
    if tracks.is_empty() {
        description.push_str("*No listen history yet — play some tracks!*");
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
            "🕐 Listen History — {total} track{}",
            if total == 1 { "" } else { "s" }
        ))
        .description(description)
        .color(0x747F8D)
        .footer(serenity::builder::CreateEmbedFooter::new(format!(
            "Page {}/{}",
            page + 1,
            pages.max(1)
        )))
}
