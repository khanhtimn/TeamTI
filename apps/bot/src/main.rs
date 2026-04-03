use serenity::Client;
use serenity::prelude::{GatewayIntents, Token};

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use adapters_discord::handler::DiscordHandler;
use adapters_media_store::fs_store::FsStore;
use adapters_media_store::scanner::MediaScanner;
use adapters_persistence::db::Database;
use adapters_persistence::migrations::run_migrations;
use adapters_persistence::repositories::track_repository::PgTrackRepository;
use adapters_voice::songbird_gateway::SongbirdPlaybackGateway;
use application::EnrichmentOrchestrator;
use application::events::AcoustIdRequest;
use application::ports::repository::TrackRepository;
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
    let config = Arc::new(config);

    info!("Connecting to database...");
    let db = Database::connect(&config.database_url).await?;
    run_migrations(&db).await?;

    info!("Initializing storage and repositories...");
    let track_repo: Arc<PgTrackRepository> = Arc::new(PgTrackRepository::new(db.clone()));

    // Startup watchdog: reset stale 'enriching' rows to 'pending'
    let reset_count = track_repo
        .reset_stale_enriching()
        .await
        .expect("stale enriching watchdog failed");
    if reset_count > 0 {
        info!(
            count = reset_count,
            "Reset stale enriching tracks to pending"
        );
    }

    let media_store = Arc::new(FsStore::new(&config.media_root));

    // ── Scan Pipeline ──────────────────────────────────────────────────
    let token = CancellationToken::new();

    // 1. Start scan pipeline — returns TrackScanned receiver and SMB semaphore
    let (scan_rx, _smb_semaphore) =
        MediaScanner::start(Arc::clone(&config), Arc::clone(&track_repo), token.clone());

    // 2. AcoustID channel — no-op consumer until Pass 3
    // Bounded channel: back-pressure intentional.
    // If consumer is slow, Orchestrator blocks until space is available.
    // Pass 3 replaces the no-op consumer with the real AcoustID adapter.
    let (acoustid_tx, mut acoustid_rx) = mpsc::channel::<AcoustIdRequest>(64);
    tokio::spawn(async move {
        while let Some(req) = acoustid_rx.recv().await {
            info!(
                "pass2 stub: pending enrich for track_id={} fingerprint_len={}",
                req.track_id,
                req.fingerprint.len(),
            );
        }
    });

    // 3. Enrichment Orchestrator
    let orchestrator = Arc::new(EnrichmentOrchestrator {
        repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        scan_interval_secs: config.scan_interval_secs,
        failed_retry_limit: config.failed_retry_limit,
        unmatched_retry_limit: config.unmatched_retry_limit,
    });
    {
        let tok = token.clone();
        let o = Arc::clone(&orchestrator);
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = o.run(scan_rx, acoustid_tx) => {}
            }
        });
    }

    // ── Discord Bot ────────────────────────────────────────────────────
    let intents = GatewayIntents::non_privileged() | GatewayIntents::GUILD_VOICE_STATES;
    let songbird = songbird::Songbird::serenity();

    info!("Initializing Application Services...");
    let playback_gateway = Arc::new(SongbirdPlaybackGateway::new(songbird.clone()));

    let join_voice = Arc::new(JoinVoice::new(playback_gateway.clone()));
    let leave_voice = Arc::new(LeaveVoice::new(playback_gateway.clone()));
    let enqueue_track = Arc::new(EnqueueTrack::new(
        playback_gateway.clone(),
        track_repo.clone(),
        media_store.clone(),
    ));

    info!("Initializing Discord Handler...");
    let handler = DiscordHandler {
        target_guild_id: config.discord_guild_id,
        join_voice,
        leave_voice,
        enqueue_track,
    };

    let token_str: Token = config.discord_token.parse()?;

    let mut client = Client::builder(token_str, intents)
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
