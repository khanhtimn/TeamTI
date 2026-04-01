use serenity::Client;
use serenity::prelude::GatewayIntents;
use songbird::SerenityInit;
use std::sync::Arc;
use tracing::{error, info};

use adapters_discord::handler::DiscordHandler;
use adapters_media_store::fs_store::FsStore;
use adapters_persistence::db::Database;
use adapters_persistence::migrations::run_migrations;
use adapters_persistence::repositories::media_repository::PgMediaRepository;
use adapters_voice::songbird_gateway::SongbirdPlaybackGateway;
use shared_config::Config;

use application::services::{
    enqueue_track::EnqueueTrack, join_voice::JoinVoice, leave_voice::LeaveVoice,
    register_media::RegisterMedia,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    shared_observability::setup();
    let config = Config::load();

    info!("Connecting to database...");
    let db = Database::connect(&config.database_url).await?;
    run_migrations(&db).await?;

    info!("Initializing storage and repositories...");
    let media_repo = Arc::new(PgMediaRepository::new(db.clone()));
    let media_store = Arc::new(FsStore::new(&config.media_root));

    let intents = GatewayIntents::non_privileged() | GatewayIntents::GUILD_VOICE_STATES;

    let songbird = songbird::Songbird::serenity();

    info!("Initializing Application Services...");
    let playback_gateway = Arc::new(SongbirdPlaybackGateway::new(songbird.clone()));

    let join_voice = Arc::new(JoinVoice::new(playback_gateway.clone()));
    let leave_voice = Arc::new(LeaveVoice::new(playback_gateway.clone()));
    let register_media = Arc::new(RegisterMedia::new(media_repo.clone(), media_store.clone()));
    let enqueue_track = Arc::new(EnqueueTrack::new(playback_gateway.clone()));

    info!("Initializing Discord Handler...");
    let handler = DiscordHandler {
        target_guild_id: config.discord_guild_id,
        join_voice,
        leave_voice,
        register_media,
        enqueue_track,
    };

    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(handler)
        .voice_manager_arc(songbird)
        .await
        .expect("Err creating client");

    info!("Starting bot...");
    if let Err(why) = client.start().await {
        error!("Client error: {:?}", why);
    }

    Ok(())
}
