use serenity::Client;
use serenity::prelude::{GatewayIntents, Token};

use std::sync::Arc;
use tracing::{error, info};

use adapters_discord::handler::DiscordHandler;
use adapters_media_store::fs_store::FsStore;
use adapters_media_store::scanner::MediaScanner;
use adapters_persistence::db::Database;
use adapters_persistence::migrations::run_migrations;
use adapters_persistence::repositories::media_repository::PgMediaRepository;
use adapters_voice::songbird_gateway::SongbirdPlaybackGateway;
use shared_config::Config;

use application::services::{
    enqueue_track::EnqueueTrack, join_voice::JoinVoice, leave_voice::LeaveVoice,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let _ = dotenvy::dotenv();
    shared_observability::setup();
    let config = Config::load();

    info!("Connecting to database...");
    let db = Database::connect(&config.database_url).await?;
    run_migrations(&db).await?;

    info!("Initializing storage and repositories...");
    let media_repo: Arc<PgMediaRepository> = Arc::new(PgMediaRepository::new(db.clone()));
    let media_store = Arc::new(FsStore::new(&config.media_root));

    // Run the initial media scan
    let scanner = Arc::new(MediaScanner::new(&config.media_root, media_repo.clone()));
    info!("Running startup media scan...");
    match scanner.scan().await {
        Ok(report) => info!(%report, "Startup scan finished"),
        Err(e) => error!("Startup scan failed: {:?}", e),
    }

    let intents = GatewayIntents::non_privileged() | GatewayIntents::GUILD_VOICE_STATES;

    let songbird = songbird::Songbird::serenity();

    info!("Initializing Application Services...");
    let playback_gateway = Arc::new(SongbirdPlaybackGateway::new(songbird.clone()));

    let join_voice = Arc::new(JoinVoice::new(playback_gateway.clone()));
    let leave_voice = Arc::new(LeaveVoice::new(playback_gateway.clone()));
    let enqueue_track = Arc::new(EnqueueTrack::new(
        playback_gateway.clone(),
        media_repo.clone(),
        media_store.clone(),
    ));

    info!("Initializing Discord Handler...");
    let handler = DiscordHandler {
        target_guild_id: config.discord_guild_id,
        join_voice,
        leave_voice,
        enqueue_track,
        search_port: media_repo.clone(),
        scanner: scanner.clone(),
    };

    let token: Token = config.discord_token.parse()?;

    let mut client = Client::builder(token, intents)
        .event_handler(Arc::new(handler))
        .voice_manager(songbird)
        .await
        .expect("Err creating client");

    info!("Starting bot...");
    if let Err(why) = client.start().await {
        error!("Client error: {:?}", why);
    }

    Ok(())
}
