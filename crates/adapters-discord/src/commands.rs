use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::model::application::CommandOptionType;

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
        .description("Play a track from the music catalog")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "query",
                "Track name to search",
            )
            .required(true)
            .set_autocomplete(true),
        )
}

pub fn scan() -> CreateCommand<'static> {
    CreateCommand::new("scan").description("Scan the media directory for new tracks (admin only)")
}
