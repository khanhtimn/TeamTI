use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::model::application::CommandOptionType;

pub fn ping() -> CreateCommand {
    CreateCommand::new("ping").description("A ping command")
}

pub fn join() -> CreateCommand {
    CreateCommand::new("join").description("Joins your current voice channel")
}

pub fn leave() -> CreateCommand {
    CreateCommand::new("leave").description("Leaves the voice channel")
}

pub fn play_local() -> CreateCommand {
    CreateCommand::new("play_local")
        .description("Plays a local file")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "path",
                "Path to the local file to play",
            )
            .required(true),
        )
}
