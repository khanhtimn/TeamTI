use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::events::{AcoustIdRequest, TrackScanned};
use crate::ports::repository::TrackRepository;
use crate::ports::search::TrackSearchPort;

pub struct EnrichmentOrchestrator {
    pub repo: Arc<dyn TrackRepository>,
    pub search_port: Arc<dyn TrackSearchPort>,
    pub scan_interval_secs: u64,
    pub failed_retry_limit: i32,
    pub unmatched_retry_limit: i32,
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
                    info!(
                        track_id = %scanned.track_id,
                        correlation_id = %scanned.correlation_id,
                        "orchestrator: reactive enrich for track"
                    );

                    // Index track immediately so it is searchable even if enrichment fails or is delayed.
                    if let Err(e) = self.search_port.reindex_track(scanned.track_id).await {
                        warn!(
                            track_id = %scanned.track_id,
                            error = %e,
                            "orchestrator: failed to reindex incoming track"
                        );
                    }

                    // CRIT-3 fix: use claim_single to atomically set status='enriching',
                    // preventing duplicate enrichment with the proactive poll path.
                    // If the poll path already claimed it, claim_single returns None.
                    let track = match self.repo.claim_single(scanned.track_id).await {
                        Ok(Some(t)) => t,
                        Ok(None) => {
                            // Already claimed by poll path, or not in 'pending' state — skip.
                            continue;
                        }
                        Err(e) => {
                            warn!(
                                track_id = %scanned.track_id,
                                correlation_id = %scanned.correlation_id,
                                error = %e,
                                "orchestrator: claim_single failed"
                            );
                            continue;
                        }
                    };
                    let _ = acoustid_tx.send(AcoustIdRequest {
                        track_id:            scanned.track_id,
                        fingerprint:         scanned.fingerprint,
                        duration_ms:         scanned.duration_ms,
                        enrichment_attempts: track.enrichment_attempts,
                        blob_location:       track.blob_location.unwrap_or_default(),
                        correlation_id:      scanned.correlation_id,
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
                    count = tracks.len(),
                    "orchestrator: claimed tracks for enrichment batch"
                );
                for track in tracks {
                    if let (Some(fp), Some(dur)) = (&track.audio_fingerprint, track.duration_ms) {
                        let _ = acoustid_tx
                            .send(AcoustIdRequest {
                                track_id: track.id,
                                fingerprint: fp.clone(),
                                duration_ms: dur,
                                enrichment_attempts: track.enrichment_attempts,
                                blob_location: track.blob_location.clone().unwrap_or_default(),
                                correlation_id: uuid::Uuid::new_v4(),
                            })
                            .await;
                    } else {
                        warn!(
                            "orchestrator: track {} missing fingerprint \
                              or duration, cannot enrich — reverting to pending",
                            track.id
                        );

                        // Prevent permanent DB lockups by transitioning state natively
                        // back to 'pending' allowing future proactive loops to discover it.
                        // F10: Increment attempts so permanently unfingerprintable files
                        // eventually hit the retry limit instead of looping forever.
                        let _ = self
                            .repo
                            .update_enrichment_status(
                                track.id,
                                &domain::EnrichmentStatus::Pending,
                                track.enrichment_attempts + 1,
                                None,
                            )
                            .await;
                    }
                }
            }
            Err(e) => warn!(error = %e, "orchestrator: claim_for_enrichment error"),
        }
    }
}
