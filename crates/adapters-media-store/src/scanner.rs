use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use adapters_persistence::repositories::track_repository::PgTrackRepository;
use adapters_watcher::MediaWatcher;
use application::events::TrackScanned;
use shared_config::Config;

use crate::classifier::{ToFingerprint, run_classifier};
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
        track_repo: Arc<PgTrackRepository>,
        token: CancellationToken,
    ) -> (mpsc::Receiver<TrackScanned>, Arc<Semaphore>) {
        let smb_semaphore = Arc::new(Semaphore::new(config.smb_read_concurrency));
        let fp_concurrency = Arc::new(Semaphore::new(config.fingerprint_concurrency));

        let (_watcher, file_rx) =
            MediaWatcher::start(Arc::clone(&config)).expect("MediaWatcher failed to start");

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
                let _keep = _watcher;
                tok.cancelled().await;
            }
        });

        (scan_rx, smb_semaphore)
    }
}

fn spawn_with_cancel<F>(token: CancellationToken, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = token.cancelled() => {}
            _ = fut => {}
        }
    });
}
