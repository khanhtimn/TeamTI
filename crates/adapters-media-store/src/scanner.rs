use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::info;

use adapters_watcher::{FileEvent, FileEventKind, MediaWatcher};
use application::events::TrackScanned;
use application::ports::repository::TrackRepository;
use shared_config::Config;

use crate::classifier::{SUPPORTED_EXTENSIONS, ToFingerprint, run_classifier};
use crate::fingerprint::run_fingerprint_worker;

pub struct MediaScanner;

impl MediaScanner {
    /// Start the scan pipeline.
    ///
    /// Returns:
    /// - `mpsc::Receiver<TrackScanned>` — connect to EnrichmentOrchestrator
    /// - `Arc<Semaphore>` — SMB_READ_SEMAPHORE, store in AppState for Pass 4
    pub fn start(
        config: Arc<Config>,
        track_repo: Arc<dyn TrackRepository>,
        token: CancellationToken,
    ) -> (mpsc::Receiver<TrackScanned>, Arc<Semaphore>) {
        let smb_semaphore = Arc::new(Semaphore::new(config.smb_read_concurrency));
        let fp_concurrency = Arc::new(Semaphore::new(config.fingerprint_concurrency));

        let (watcher, watcher_rx) =
            MediaWatcher::start(Arc::clone(&config)).expect("MediaWatcher failed to start");

        // Create a merged channel: initial scan + ongoing watcher events
        let (merged_tx, file_rx) = mpsc::channel::<FileEvent>(2048);

        // Spawn initial directory walk — feeds existing files into the pipeline
        // immediately, without waiting for the PollWatcher's first poll cycle.
        {
            let tx = merged_tx.clone();
            let media_root = config.media_root.clone();
            let tok = token.clone();
            tokio::spawn(async move {
                run_initial_scan(&media_root, tx, tok).await;
            });
        }

        // Forward ongoing watcher events into the merged channel
        {
            let tx = merged_tx;
            tokio::spawn(async move {
                forward_watcher_events(watcher_rx, tx).await;
            });
        }

        let (fp_tx, fp_rx) = mpsc::channel::<ToFingerprint>(256);
        let (scan_tx, scan_rx) = mpsc::channel::<TrackScanned>(128);

        // Classifier task
        spawn_with_cancel(token.clone(), {
            let config = Arc::clone(&config);
            let repo = Arc::clone(&track_repo);
            async move { run_classifier(config, repo, file_rx, fp_tx).await }
        });

        // Fingerprint Worker task
        spawn_with_cancel(token.clone(), {
            let config = Arc::clone(&config);
            let repo = Arc::clone(&track_repo);
            let smb = Arc::clone(&smb_semaphore);
            let fpc = Arc::clone(&fp_concurrency);
            async move { run_fingerprint_worker(config, repo, smb, fpc, fp_rx, scan_tx).await }
        });

        // Keep watcher handle alive until cancellation
        tokio::spawn({
            let tok = token;
            async move {
                let _keep = watcher;
                tok.cancelled().await;
            }
        });

        (scan_rx, smb_semaphore)
    }
}

/// Walk the media directory recursively and send CreateOrModify events
/// for all supported audio files. This ensures tracks are discovered
/// immediately on startup without waiting for the PollWatcher's first cycle.
async fn run_initial_scan(
    media_root: &std::path::Path,
    tx: mpsc::Sender<FileEvent>,
    token: CancellationToken,
) {
    use std::collections::HashSet;

    let supported: HashSet<&str> = SUPPORTED_EXTENSIONS.iter().copied().collect();
    let mut count: usize = 0;

    // Use blocking walkdir in a spawn_blocking to avoid starving the runtime
    let root = media_root.to_path_buf();
    let (walk_tx, mut walk_rx) = mpsc::channel::<std::path::PathBuf>(256);

    let walk_handle = tokio::task::spawn_blocking(move || {
        let walker = walkdir::WalkDir::new(&root)
            .follow_links(true)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_file());

        for entry in walker {
            if walk_tx.blocking_send(entry.into_path()).is_err() {
                break; // receiver dropped
            }
        }
    });

    while let Some(path) = walk_rx.recv().await {
        if token.is_cancelled() {
            break;
        }

        let is_audio = path
            .extension()
            .and_then(|ex| ex.to_str())
            .map(str::to_lowercase)
            .is_some_and(|ex| supported.contains(ex.as_str()));

        if !is_audio {
            continue;
        }

        if tx
            .send(FileEvent {
                path,
                kind: FileEventKind::CreateOrModify,
            })
            .await
            .is_err()
        {
            break; // pipeline shut down
        }
        count += 1;
    }

    // Wait for walk to finish (it may already be done)
    let _ = walk_handle.await;
    info!(count, "initial scan: discovered audio files");
}

/// Forward events from the watcher's receiver into the merged channel.
async fn forward_watcher_events(
    mut watcher_rx: mpsc::Receiver<FileEvent>,
    tx: mpsc::Sender<FileEvent>,
) {
    while let Some(event) = watcher_rx.recv().await {
        if tx.send(event).await.is_err() {
            break;
        }
    }
}

fn spawn_with_cancel<F>(token: CancellationToken, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = token.cancelled() => {}
            () = fut => {}
        }
    });
}
