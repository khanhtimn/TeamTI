//! Reusable pagination component for Discord embeds.
//!
//! All paginated views (/playlist view, /favourite list, /history) use this
//! shared pattern: embed + ◀/▶ buttons, page state in button custom_id,
//! 5-minute timeout.

use serenity::builder::{CreateActionRow, CreateButton, CreateComponent};
use serenity::model::application::ComponentInteraction;
use uuid::Uuid;

/// Custom ID format: `"{view_type}:{resource_id}:{page}:{user_id}"`
pub fn make_custom_id(view_type: &str, resource_id: &str, page: i64, user_id: &str) -> String {
    format!("{view_type}:{resource_id}:{page}:{user_id}")
}

/// Parse a pagination custom_id. Returns (view_type, resource_id, page, user_id).
pub fn parse_custom_id(custom_id: &str) -> Option<(String, String, i64, String)> {
    let parts: Vec<&str> = custom_id.splitn(4, ':').collect();
    if parts.len() != 4 {
        return None;
    }
    let page: i64 = parts[2].parse().ok()?;
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        page,
        parts[3].to_string(),
    ))
}

/// Build the navigation button row as a `CreateComponent::ActionRow`.
pub fn build_nav_buttons<'a>(
    view_type: &str,
    resource_id: &str,
    current_page: i64,
    total_pages: i64,
    user_id: &str,
) -> CreateComponent<'a> {
    let prev_disabled = current_page <= 0;
    let next_disabled = current_page >= total_pages - 1;

    let prev_btn = CreateButton::new(make_custom_id(
        view_type,
        resource_id,
        current_page.saturating_sub(1),
        user_id,
    ))
    .label("◀ Prev")
    .style(serenity::all::ButtonStyle::Secondary)
    .disabled(prev_disabled);

    let page_indicator = CreateButton::new(format!("{view_type}_page_indicator:{resource_id}"))
        .label(format!("Page {}/{}", current_page + 1, total_pages.max(1)))
        .style(serenity::all::ButtonStyle::Secondary)
        .disabled(true);

    let next_btn = CreateButton::new(make_custom_id(
        view_type,
        resource_id,
        current_page + 1,
        user_id,
    ))
    .label("Next ▶")
    .style(serenity::all::ButtonStyle::Secondary)
    .disabled(next_disabled);

    CreateComponent::ActionRow(CreateActionRow::Buttons(
        vec![prev_btn, page_indicator, next_btn].into(),
    ))
}

/// Compute total pages from total items and page size.
pub fn total_pages(total_items: i64, page_size: i64) -> i64 {
    if total_items == 0 {
        1
    } else {
        (total_items + page_size - 1) / page_size
    }
}

/// Check if the interacting user owns this pagination session.
pub fn is_session_owner(interaction: &ComponentInteraction, session_user_id: &str) -> bool {
    interaction.user.id.to_string() == session_user_id
}

/// Send an ephemeral "not your session" response.
pub async fn send_not_yours(http: &serenity::all::Http, interaction: &ComponentInteraction) {
    use serenity::builder::{CreateInteractionResponse, CreateInteractionResponseMessage};
    let resp = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content("This isn't your session — run the command yourself.")
            .ephemeral(true),
    );
    let _ = interaction.create_response(http, resp).await;
}

/// UUID helper for resource_id parsing
pub fn parse_resource_uuid(resource_id: &str) -> Option<Uuid> {
    Uuid::parse_str(resource_id).ok()
}

/// Page size constant for all paginated views
pub const PAGE_SIZE: i64 = 10;
