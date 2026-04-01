pub struct Config {
    pub database_url: String,
    pub discord_token: String,
    pub discord_guild_id: u64,
    pub media_root: String,
}

impl Config {
    pub fn load() -> Self {
        let _ = dotenvy::dotenv();
        
        Self {
            database_url: std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            discord_token: std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set"),
            discord_guild_id: std::env::var("DISCORD_GUILD_ID")
                .expect("DISCORD_GUILD_ID must be set")
                .parse()
                .expect("DISCORD_GUILD_ID must be a valid u64"),
            media_root: std::env::var("MEDIA_ROOT").unwrap_or_else(|_| "./media_data".to_string()),
        }
    }
}
