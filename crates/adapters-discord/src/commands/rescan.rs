use std::sync::Arc;

use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;

use application::ports::MusicSearchPort;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("rescan")
        .description("Trigger a library rescan. Admin only.")
        .default_member_permissions(Permissions::ADMINISTRATOR)
}

pub async fn run(
    http: &serenity::all::Http,
    interaction: &CommandInteraction,
    search_port: &Arc<dyn MusicSearchPort>,
) {
    let _ = interaction.defer_ephemeral(http).await;

    tracing::info!(
        user_id    = %interaction.user.id,
        guild_id   = %interaction.guild_id.unwrap_or_default(),
        operation  = "rescan.requested",
        "admin triggered rescan"
    );

    // Rebuild the Tantivy search index from PostgreSQL
    match search_port.rebuild_index().await {
        Ok(count) => {
            tracing::info!(
                documents = count,
                operation = "search.rescan_rebuild_complete",
                "Tantivy index rebuilt after rescan"
            );
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(format!(
                        "✅ Search index rebuilt ({count} tracks indexed).\n\
                         New files will be processed on next scan cycle."
                    )),
                )
                .await;
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                operation = "search.rescan_rebuild_failed",
                "Tantivy index rebuild failed during rescan"
            );
            let _ = interaction
                .edit_response(
                    http,
                    EditInteractionResponse::new().content(
                        "⚠️ Search index rebuild failed. Check logs for details.\n\
                         New files will be processed on next scan cycle.",
                    ),
                )
                .await;
        }
    }
}
