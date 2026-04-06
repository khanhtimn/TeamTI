//! Background worker: analyses tracks with bliss-audio to produce feature vectors.
//!
//! This worker runs independently from the enrichment pipeline.
//! It polls for tracks with `analysis_status = 'pending'` (or 'failed' with
//! attempts < max), claims them, and runs bliss-audio analysis via `spawn_blocking`.

use std::sync::Arc;
use tokio::task::JoinSet;
use tokio::time::{Duration, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::AppError;
use crate::ports::AudioAnalysisPort;
use crate::ports::repository::TrackRepository;

pub struct AnalysisWorker {
    pub port: Arc<dyn AudioAnalysisPort>,
    pub repo: Arc<dyn TrackRepository>,
    pub media_root: std::path::PathBuf,
    pub concurrency: usize,
    pub poll_interval_secs: u64,
    pub max_attempts: i32,
}

impl AnalysisWorker {
    pub async fn run(&self) {
        info!(
            concurrency = self.concurrency,
            poll_secs = self.poll_interval_secs,
            operation = "analysis_worker.start",
            "Analysis worker started"
        );

        loop {
            match self.poll_batch().await {
                Ok(0) => {
                    // No work — backoff
                    sleep(Duration::from_secs(self.poll_interval_secs)).await;
                }
                Ok(n) => {
                    info!(
                        analysed = n,
                        operation = "analysis_worker.batch_done",
                        "Analysis batch completed"
                    );
                    // Immediately try another batch
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        operation = "analysis_worker.poll_error",
                        "Analysis poll error, backing off"
                    );
                    sleep(Duration::from_secs(self.poll_interval_secs * 2)).await;
                }
            }
        }
    }

    async fn poll_batch(&self) -> Result<usize, AppError> {
        let tracks = self
            .repo
            .claim_for_analysis(self.concurrency as i64)
            .await?;

        if tracks.is_empty() {
            return Ok(0);
        }

        let batch_size = tracks.len();
        let mut join_set = JoinSet::new();

        for track in tracks {
            let port = Arc::clone(&self.port);
            let repo = Arc::clone(&self.repo);
            let media_root = self.media_root.clone();
            let track_id = track.id;
            let blob_location = track.blob_location.clone();

            join_set.spawn(async move {
                analyse_one(port, repo, media_root, track_id, &blob_location).await
            });
        }

        // Await all in-flight tasks (bounded by claim limit)
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    // Already handled in analyse_one
                    tracing::debug!(error = %e, "analysis task returned error (already persisted)");
                }
                Err(join_err) => {
                    warn!(error = %join_err, "analysis task panicked");
                }
            }
        }

        Ok(batch_size)
    }
}

async fn analyse_one(
    port: Arc<dyn AudioAnalysisPort>,
    repo: Arc<dyn TrackRepository>,
    media_root: std::path::PathBuf,
    track_id: Uuid,
    blob_location: &str,
) -> Result<(), AppError> {
    let full_path = media_root.join(blob_location);
    let full_path_str = full_path.to_string_lossy().to_string();

    match port.analyse_track(&full_path_str).await {
        Ok(vector) => {
            if let Err(e) = repo.update_analysis_done(track_id, &vector).await {
                warn!(
                    track_id = %track_id,
                    error = %e,
                    operation = "analysis_worker.store_failed",
                    "Failed to store bliss vector"
                );
                return Err(e);
            }
            tracing::debug!(
                track_id = %track_id,
                operation = "analysis_worker.done",
                "Analysis completed"
            );
            Ok(())
        }
        Err(e) => {
            warn!(
                track_id = %track_id,
                error = %e,
                operation = "analysis_worker.analyse_failed",
                "bliss analysis failed"
            );
            let _ = repo.update_analysis_failed(track_id).await;
            Err(e)
        }
    }
}
