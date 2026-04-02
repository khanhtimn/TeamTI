use serenity::Error;
use serenity::builder::{CreateInteractionResponse, CreateInteractionResponseMessage};
use serenity::model::application::CommandInteraction;
use serenity::prelude::Context;

pub async fn respond_success(
    ctx: &Context,
    command: &CommandInteraction,
    message: &str,
) -> Result<(), Error> {
    let data = CreateInteractionResponseMessage::new().content(message);
    let builder = CreateInteractionResponse::Message(data);
    command.create_response(&ctx.http, builder).await
}

pub async fn respond_error(
    ctx: &Context,
    command: &CommandInteraction,
    message: &str,
) -> Result<(), Error> {
    let data = CreateInteractionResponseMessage::new().content(format!("❌ {}", message));
    let builder = CreateInteractionResponse::Message(data);
    command.create_response(&ctx.http, builder).await
}
