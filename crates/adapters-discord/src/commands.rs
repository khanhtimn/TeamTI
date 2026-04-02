use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::model::application::CommandOptionType;
use serenity::model::Permissions;

pub fn ping() -> CreateCommand<'static> {
    CreateCommand::new("ping").description("A ping command")
}

pub fn join() -> CreateCommand<'static> {
    CreateCommand::new("join").description("Joins your current voice channel")
}

pub fn leave() -> CreateCommand<'static> {
    CreateCommand::new("leave").description("Leaves the voice channel")
}

pub fn play() -> CreateCommand<'static> {
    CreateCommand::new("play")
        .description("Search and queue a track")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "query",
                "Track title to search",
            )
            .required(true)
            .set_autocomplete(true),
        )
}

pub fn scan() -> CreateCommand<'static> {
    CreateCommand::new("scan")
        .description("Scan and index local media files (admin only)")
        .default_member_permissions(Permissions::ADMINISTRATOR)
}
