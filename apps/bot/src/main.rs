use serenity::Client;
use serenity::prelude::{GatewayIntents, Token};

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use adapters_acoustid::AcoustIdAdapter;
use adapters_cover_art::CoverArtAdapter;
use adapters_discord::handler::DiscordEventHandler;
use adapters_discord::lifecycle_worker::run_lifecycle_worker;
use adapters_lyrics::LyricsAdapter;
use adapters_media_store::fs_store::FsStore;
use adapters_media_store::scanner::MediaScanner;
use adapters_media_store::tag_writer_port::FileTagWriterAdapter;
use adapters_musicbrainz::MusicBrainzAdapter;
use adapters_persistence::db::Database;
use adapters_persistence::migrations::run_migrations;
use adapters_persistence::repositories::album_repository::PgAlbumRepository;
use adapters_persistence::repositories::artist_repository::PgArtistRepository;
use adapters_persistence::repositories::playlist_repository::PgPlaylistRepository;
use adapters_persistence::repositories::recommendation_repository::PgRecommendationRepository;
use adapters_persistence::repositories::track_repository::PgTrackRepository;
use adapters_persistence::repositories::user_library_repository::PgUserLibraryRepository;
use adapters_search::TantivySearchAdapter;
use adapters_voice::state_map::GuildStateMap;
use application::events::{ToCoverArt, ToLyrics, ToMusicBrainz, ToTagWriter};
use application::ports::playlist::PlaylistPort;
use application::ports::recommendation::RecommendationPort;
use application::ports::repository::{AlbumRepository, ArtistRepository, TrackRepository};
use application::ports::search::TrackSearchPort;
use application::ports::user_library::UserLibraryPort;
use application::tag_writer_worker::run_startup_tag_poller;
use application::{
    AcoustIdWorker, CoverArtWorker, EnrichmentOrchestrator, LyricsWorker, MusicBrainzWorker,
    TagWriterWorker,
};
use shared_config::Config;

mod telemetry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let _ = dotenvy::dotenv();
    telemetry::init_tracing();
    let config = Config::load();
    config.validate().expect("Invalid configuration");
    let config = Arc::new(config);

    info!("Connecting to database...");
    let db = Database::connect(&config.database_url, config.db_pool_size).await?;
    run_migrations(&db).await?;

    info!("Initializing storage and repositories...");
    let track_repo: Arc<PgTrackRepository> = Arc::new(PgTrackRepository::new(db.clone()));
    let artist_repo: Arc<PgArtistRepository> = Arc::new(PgArtistRepository::new(db.clone()));
    let album_repo: Arc<PgAlbumRepository> = Arc::new(PgAlbumRepository::new(db.clone()));

    // ── Pass 3: User layer repositories ────────────────────────────────
    let playlist_port: Arc<dyn PlaylistPort> = Arc::new(PgPlaylistRepository::new(db.clone()));
    let user_library_port: Arc<dyn UserLibraryPort> =
        Arc::new(PgUserLibraryRepository::new(db.clone()));
    let recommendation_port: Arc<dyn RecommendationPort> =
        Arc::new(PgRecommendationRepository::new(db.clone()));

    let acoustid_adapter = Arc::new(AcoustIdAdapter::new(config.acoustid_api_key.clone()));
    let mb_adapter = Arc::new(MusicBrainzAdapter::new(config.user_agent.clone()));
    let lyrics_adapter = Arc::new(LyricsAdapter::new(
        config.media_root.clone(),
        config.user_agent.clone(),
    ));
    let cover_art_adapter = Arc::new(CoverArtAdapter::new(config.cover_art_concurrency));
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

    let _media_store = Arc::new(FsStore::new(&config.media_root));

    // ── Tantivy Search Index ───────────────────────────────────────────
    // 1. NAS path guard
    if config.tantivy_index_path.starts_with(&config.media_root) {
        tracing::warn!(
            operation = "search.startup_check",
            path = %config.tantivy_index_path.display(),
            "TANTIVY_INDEX_PATH is under MEDIA_ROOT — index must be on local disk, not NAS"
        );
    }

    // 2. Open or create the index (synchronous)
    let search_port: Arc<dyn TrackSearchPort> = Arc::new(
        TantivySearchAdapter::open_or_create(config.tantivy_index_path.clone(), db.0.clone())
            .expect("failed to open Tantivy search index"),
    );

    // 3. Full rebuild before workers or Discord client start
    let t0 = std::time::Instant::now();
    let doc_count = search_port
        .rebuild_index()
        .await
        .expect("failed to build Tantivy index from PostgreSQL");

    tracing::info!(
        documents = doc_count,
        elapsed_ms = t0.elapsed().as_millis(),
        operation = "search.startup_rebuild_complete",
        "Tantivy search index ready"
    );

    // ── Scan Pipeline ──────────────────────────────────────────────────
    let token = CancellationToken::new();

    // 1. Start scan pipeline — returns TrackScanned receiver and SMB semaphore
    let (scan_rx, _smb_semaphore) = MediaScanner::start(
        Arc::clone(&config),
        Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        token.clone(),
    );

    let (acoustid_tx, acoustid_rx) = mpsc::channel(64);
    let (mb_tx, mb_rx) = mpsc::channel::<ToMusicBrainz>(64);
    let (lyrics_tx, lyrics_rx) = mpsc::channel::<ToLyrics>(64);
    let (cover_tx, cover_rx) = mpsc::channel::<ToCoverArt>(64);
    let (tag_writer_tx, tag_writer_rx) = mpsc::channel::<ToTagWriter>(128);

    {
        let worker = Arc::new(AcoustIdWorker {
            port: acoustid_adapter,
            repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
            confidence_threshold: config.enrichment_confidence_threshold,
            failed_retry_limit: config.failed_retry_limit,
            unmatched_retry_limit: config.unmatched_retry_limit,
        });
        let tok = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = worker.run(acoustid_rx, mb_tx) => {}
            }
        });
    }

    {
        let worker = Arc::new(MusicBrainzWorker {
            port: mb_adapter,
            track_repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
            artist_repo: Arc::clone(&artist_repo) as Arc<dyn ArtistRepository>,
            album_repo: Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
            failed_retry_limit: config.failed_retry_limit,
            fetch_work_credits: config.mb_fetch_work_credits,
        });
        let tok = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = worker.run(mb_rx, lyrics_tx) => {}
            }
        });
    }

    {
        let worker = Arc::new(LyricsWorker {
            port: lyrics_adapter,
            track_repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        });
        let tok = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = worker.run(lyrics_rx, cover_tx) => {}
            }
        });
    }

    {
        let worker = Arc::new(CoverArtWorker {
            port: cover_art_adapter,
            track_repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
            album_repo: Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
            media_root: config.media_root.clone(),
            tag_writer_tx: tag_writer_tx.clone(),
        });
        let tok = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = worker.run(cover_rx) => {}
            }
        });
    }

    // 4. Tag Writer Worker
    {
        let file_tag_writer = Arc::new(FileTagWriterAdapter {
            media_root: config.media_root.clone(),
            smb_semaphore: Arc::clone(&_smb_semaphore),
        });
        let worker = Arc::new(TagWriterWorker {
            tag_writer: file_tag_writer,
            track_repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
            album_repo: Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
            artist_repo: Arc::clone(&artist_repo) as Arc<dyn ArtistRepository>,
            search_port: Arc::clone(&search_port),
            task_semaphore: Arc::new(tokio::sync::Semaphore::new(config.tag_write_concurrency)),
        });
        let tok = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = worker.run(tag_writer_rx) => {}
            }
        });
    }

    // 5. Startup tag poller — drains backlog then polls periodically
    {
        let repo = Arc::clone(&track_repo) as Arc<dyn TrackRepository>;
        let tx = tag_writer_tx.clone();
        let tok = token.clone();
        let secs = config.scan_interval_secs * 4; // poll every ~20 min default
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = run_startup_tag_poller(repo, tx, secs) => {}
            }
        });
    }

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
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::GUILD_MESSAGES;

    let state_map: Arc<GuildStateMap> = Arc::new(dashmap::DashMap::new());
    let songbird_instance = songbird::Songbird::serenity();

    // ── Pass 3: Lifecycle Worker ───────────────────────────────────────
    let (lifecycle_tx, lifecycle_rx) = tokio::sync::mpsc::unbounded_channel();

    let handler = DiscordEventHandler {
        discord_guild_id: config.discord_guild_id,
        track_repo: Arc::clone(&track_repo),
        search_port: Arc::clone(&search_port),
        playlist_port: Arc::clone(&playlist_port),
        user_library_port: Arc::clone(&user_library_port),
        recommendation_port: Arc::clone(&recommendation_port),
        media_root: config.media_root.clone(),
        auto_leave_secs: config.auto_leave_secs,
        songbird: songbird_instance.clone(),
        guild_state: Arc::clone(&state_map),
        lifecycle_tx: lifecycle_tx.clone(),
    };

    let token_str: Token = config.discord_token.parse().unwrap();
    let mut client = Client::builder(token_str, intents)
        .event_handler(Arc::new(handler.clone()))
        .voice_manager(songbird_instance.clone() as Arc<dyn serenity::all::VoiceGatewayManager>)
        .await
        .expect("failed to create Discord client");

    // Close dangling listen events from previous crashes
    if let Err(e) = user_library_port.close_dangling_events(0).await {
        tracing::warn!("Failed to close dangling listen events: {}", e);
    }

    {
        let ulp = Arc::clone(&user_library_port);
        let rp = Arc::clone(&recommendation_port);
        let gs = Arc::clone(&state_map);
        let sb = songbird_instance.clone();
        let tok = token.clone();
        let http_clone = Arc::clone(&client.http);
        let cache_clone = Arc::clone(&client.cache);
        let media = config.media_root.clone();
        let als = config.auto_leave_secs;
        let ltx = lifecycle_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = tok.cancelled() => {}
                _ = run_lifecycle_worker(lifecycle_rx, ulp, rp, gs, sb, http_clone, cache_clone, media, als, ltx) => {}
            }
        });
    }

    // Start the Discord client (blocks until shutdown signal)
    let shutdown_trigger = client.shard_manager.get_shutdown_trigger();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Could not register ctrl+c handler");
        token.cancel();
        shutdown_trigger();
    });

    client.start().await.expect("Discord client crashed");

    Ok(())
}
