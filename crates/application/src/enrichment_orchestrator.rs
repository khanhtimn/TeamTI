use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::events::{AcoustIdRequest, TrackScanned};
use crate::ports::repository::TrackRepository;

pub struct EnrichmentOrchestrator {
    pub repo: Arc<dyn TrackRepository>,
    pub scan_interval_secs: u64,
    pub failed_retry_limit: u32,
    pub unmatched_retry_limit: u32,
}

impl EnrichmentOrchestrator {
    pub async fn run(
        self: Arc<Self>,
        mut scan_rx: mpsc::Receiver<TrackScanned>,
        acoustid_tx: mpsc::Sender<AcoustIdRequest>,
    ) {
        // Immediate initial poll on startup
        self.poll_and_emit(&acoustid_tx).await;

        let mut interval = tokio::time::interval(Duration::from_secs(self.scan_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;

                // Reactive: new track from Fingerprint Worker
                Some(scanned) = scan_rx.recv() => {
                    info!("orchestrator: reactive enrich for track {}", scanned.track_id);
                    let _ = acoustid_tx.send(AcoustIdRequest {
                        track_id:      scanned.track_id,
                        fingerprint:   scanned.fingerprint,
                        duration_secs: scanned.duration_secs,
                    }).await;
                }

                // Proactive: DB poll for retryable tracks
                _ = interval.tick() => {
                    self.poll_and_emit(&acoustid_tx).await;
                }
            }
        }
    }

    async fn poll_and_emit(&self, acoustid_tx: &mpsc::Sender<AcoustIdRequest>) {
        let claimed = self
            .repo
            .claim_for_enrichment(self.failed_retry_limit, self.unmatched_retry_limit, 50)
            .await;

        match claimed {
            Ok(tracks) => {
                info!(
                    "orchestrator: claimed {} tracks for enrichment",
                    tracks.len()
                );
                for track in tracks {
                    match (&track.audio_fingerprint, track.duration_ms) {
                        (Some(fp), Some(dur)) => {
                            let _ = acoustid_tx
                                .send(AcoustIdRequest {
                                    track_id: track.id,
                                    fingerprint: fp.clone(),
                                    duration_secs: (dur / 1000) as u32,
                                })
                                .await;
                        }
                        _ => {
                            warn!(
                                "orchestrator: track {} missing fingerprint \
                                 or duration, cannot enrich — skipping",
                                track.id
                            );
                        }
                    }
                }
            }
            Err(e) => warn!("orchestrator: claim_for_enrichment error: {e}"),
        }
    }
}
