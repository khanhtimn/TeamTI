use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::events::{AcoustIdRequest, ToMusicBrainz};
use crate::ports::{AcoustIdPort, TrackRepository};

pub struct AcoustIdWorker {
    pub port: Arc<dyn AcoustIdPort>,
    pub repo: Arc<dyn TrackRepository>,
    pub confidence_threshold: f32,
    pub failed_retry_limit: i32,
    pub unmatched_retry_limit: i32,
}

impl AcoustIdWorker {
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<AcoustIdRequest>,
        mb_tx: mpsc::Sender<ToMusicBrainz>,
    ) {
        while let Some(req) = rx.recv().await {
            // Rate limiting is enforced inside the port (governor GCRA).
            let result = self
                .port
                .lookup(&crate::ports::enrichment::AudioFingerprint {
                    fingerprint: req.fingerprint.clone(),
                    duration_ms: req.duration_ms,
                })
                .await;

            // D1 fix: enrichment_attempts carried through AcoustIdRequest —
            // no need for a separate find_by_id DB call.
            let attempts = req.enrichment_attempts + 1;

            match result {
                Ok(Some(m)) if m.score >= self.confidence_threshold => {
                    info!(
                        track_id = %req.track_id,
                        correlation_id = %req.correlation_id,
                        score = m.score,
                        mbid = %m.recording_mbid,
                        "acoustid: matched track"
                    );
                    // A1 fix: persist AcoustID result immediately — crash-safe
                    // durability before the MusicBrainz stage begins.
                    if let Err(e) = self
                        .repo
                        .update_acoustid_match(req.track_id, &m.acoustid_id, m.score)
                        .await
                    {
                        warn!(
                            track_id = %req.track_id,
                            "acoustid: failed to save fp state: {e}"
                        );
                    }
                    // B2 fix: removed redundant update_enrichment_status call.
                    // The track is already 'enriching' from claim_for_enrichment,
                    // and the acoustid_id/confidence write above is sufficient.
                    let _ = mb_tx
                        .send(ToMusicBrainz {
                            track_id: req.track_id,
                            mbid: m.recording_mbid,
                            acoustid_id: m.acoustid_id,
                            confidence: m.score,
                            duration_ms: req.duration_ms,
                            blob_location: req.blob_location, // D2 carry-through
                            enrichment_attempts: attempts,    // DESIGN-3 carry-through
                            correlation_id: req.correlation_id,
                        })
                        .await;
                }

                Ok(Some(m)) => {
                    // Score below threshold — low confidence
                    warn!(
                        track_id = %req.track_id,
                        correlation_id = %req.correlation_id,
                        score = m.score,
                        "acoustid: low confidence match"
                    );
                    let status = if attempts >= self.failed_retry_limit {
                        domain::EnrichmentStatus::Exhausted
                    } else {
                        domain::EnrichmentStatus::LowConfidence
                    };
                    let _ = self
                        .repo
                        .update_enrichment_status(
                            req.track_id,
                            &status,
                            attempts,
                            Some(chrono::Utc::now()),
                        )
                        .await;
                }

                Ok(None) => {
                    // No results
                    warn!(track_id = %req.track_id, "acoustid: no match found");
                    let status = if attempts >= self.unmatched_retry_limit {
                        domain::EnrichmentStatus::Exhausted
                    } else {
                        domain::EnrichmentStatus::Unmatched
                    };
                    let _ = self
                        .repo
                        .update_enrichment_status(
                            req.track_id,
                            &status,
                            attempts,
                            Some(chrono::Utc::now()),
                        )
                        .await;
                }

                Err(e) => {
                    warn!(track_id = %req.track_id, error = %e, "acoustid: lookup error");
                    let status = if attempts >= self.failed_retry_limit {
                        domain::EnrichmentStatus::Exhausted
                    } else {
                        domain::EnrichmentStatus::Failed
                    };
                    let _ = self
                        .repo
                        .update_enrichment_status(
                            req.track_id,
                            &status,
                            attempts,
                            Some(chrono::Utc::now()),
                        )
                        .await;
                }
            }
        }
    }
}
