use serenity::builder::{CreateCommand, EditInteractionResponse};
use serenity::model::application::CommandInteraction;
use serenity::model::permissions::Permissions;

pub fn register() -> CreateCommand<'static> {
    CreateCommand::new("rescan")
        .description("Trigger a library rescan. Admin only.")
        .default_member_permissions(Permissions::ADMINISTRATOR)
}

pub async fn run(http: &serenity::all::Http, interaction: &CommandInteraction) {
    let _ = interaction.defer_ephemeral(http).await;

    tracing::info!(
        user_id    = %interaction.user.id,
        guild_id   = %interaction.guild_id.unwrap_or_default(),
        operation  = "rescan.requested",
        "admin triggered rescan"
    );

    let _ = interaction
        .edit_response(
            http,
            EditInteractionResponse::new().content(
                "Rescan requested. New files will be processed on next scan cycle.\n\
                      *(This command will be removed in a future version.)*",
            ),
        )
        .await;
}
