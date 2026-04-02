use crate::commands;
use serenity::Error;
use serenity::http::Http;
use serenity::model::id::GuildId;

pub async fn register_guild_commands(http: &Http, guild_id: GuildId) -> Result<(), Error> {
    guild_id
        .set_commands(
            http,
            &[
                commands::ping(),
                commands::join(),
                commands::leave(),
                commands::play(),
                commands::scan(),
            ],
        )
        .await?;

    Ok(())
}
