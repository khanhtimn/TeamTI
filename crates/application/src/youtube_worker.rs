//! Background worker: downloads YouTube audio via yt-dlp.
//!
//! Uses a `tokio::sync::Semaphore` for download concurrency bounding.
//! `schedule()` spawns a task immediately; the task blocks on
//! `semaphore.acquire()` until a slot is free.
//!
//! An `in_flight` `DashSet` guards against concurrent duplicate spawns
//! for the same video_id (F1 race condition fix).

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashSet;
use tracing::{info, warn};

use crate::ports::youtube::YoutubeRepository;
use crate::ports::ytdlp::YtDlpPort;

/// Manages background downloads for YouTube tracks.
#[derive(Clone)]
pub struct YoutubeDownloadWorker {
    pub ytdlp: Arc<dyn YtDlpPort>,
    pub repo: Arc<dyn YoutubeRepository>,
    pub semaphore: Arc<tokio::sync::Semaphore>,
    pub media_root: PathBuf,
    pub max_attempts: u32,
    /// Tracks video_ids that are currently being downloaded.
    /// Prevents duplicate spawns when the same video_id is scheduled
    /// multiple times before the first download completes.
    pub in_flight: Arc<DashSet<String>>,
}

impl YoutubeDownloadWorker {
    /// Schedule a download for video_id.
    /// Returns immediately — the download runs in a background task.
    /// Idempotent: calling for a video_id already in-flight is a no-op.
    pub fn schedule(&self, video_id: String, blob_path: String) {
        // F1 fix: atomically check-and-insert; bail if already in-flight
        if !self.in_flight.insert(video_id.clone()) {
            tracing::debug!(
                video_id,
                "download already in-flight, skipping duplicate spawn"
            );
            return;
        }

        let worker = self.clone();
        tokio::spawn(async move {
            worker.run_download(video_id, blob_path).await;
        });
    }

    async fn run_download(&self, video_id: String, blob_path: String) {
        // Ensure we always remove from in_flight when done, regardless of outcome
        let _guard = InFlightGuard {
            set: Arc::clone(&self.in_flight),
            key: video_id.clone(),
        };

        // 1. Acquire semaphore permit (blocks if at concurrency limit)
        let Ok(_) = self.semaphore.acquire().await else {
            return;
        };

        // 2. Check if still needed (another task may have raced and completed it)
        match self.repo.get_download_job(&video_id).await {
            Ok(Some(ref j)) if j.status == "done" => return,
            Ok(Some(ref j)) if j.status == "permanently_failed" => return,
            _ => {}
        }

        // 3. Mark as 'downloading' — returns false if another worker already claimed it
        match self.repo.lock_download_job(&video_id).await {
            Ok(false) => {
                tracing::debug!(video_id, "download job already claimed by another worker");
                return;
            }
            Err(e) => {
                warn!(video_id, error = %e, "failed to lock download job");
                return;
            }
            Ok(true) => {} // Successfully claimed
        }

        // 4. Look up track to check if it's an incomplete flat-playlist stub
        let mut actual_blob_path = blob_path.clone();
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        if let Ok(Some(track)) = self.repo.find_track_by_video_id(&video_id).await
            && track.youtube_uploader.is_none()
        {
            // Repair flat-playlist stub
            tracing::info!(video_id, "Incomplete stub detected, repairing metadata");
            if let Ok(meta) = self.ytdlp.fetch_video_metadata(&url).await {
                let _ = self
                    .repo
                    .update_youtube_stub_metadata(&video_id, &meta)
                    .await;

                // Recompute blob path with correct uploader
                let uploader = meta.uploader.as_deref().unwrap_or("unknown");
                let title = meta.title.as_deref().unwrap_or("unknown");
                actual_blob_path = self.ytdlp.compute_blob_path(uploader, title, &video_id);
            }
        }

        // 5. Construct absolute output path
        let output_path = self.media_root.join(&actual_blob_path);
        if let Some(parent) = output_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        // 6. Run yt-dlp download
        match self.ytdlp.download_audio(&url, &output_path).await {
            Ok(()) => {
                if let Err(e) = self
                    .repo
                    .complete_download_job(&video_id, &actual_blob_path)
                    .await
                {
                    tracing::error!(video_id, error = %e, "download complete but DB update failed");
                } else {
                    info!(
                        video_id,
                        blob_path = actual_blob_path,
                        "youtube download complete"
                    );
                    // Enrichment and analysis workers will pick this up automatically
                    // now that blob_location IS NOT NULL.
                }
            }
            Err(e) => {
                warn!(video_id, error = %e, "youtube download failed");
                let job = self.repo.get_download_job(&video_id).await.ok().flatten();
                let attempts = job.map_or(0, |j| j.attempts);

                if attempts + 1 >= self.max_attempts as i32 {
                    // 7a. Permanently fail
                    tracing::error!(video_id, "youtube download permanently failed");
                    let _ = self.repo.permanently_fail_download_job(&video_id).await;
                } else {
                    // 7b. Mark failed for retry
                    let _ = self.repo.fail_download_job(&video_id, &e.to_string()).await;
                }
            }
        }
    }
}

/// RAII guard that removes a video_id from the in-flight set on drop.
struct InFlightGuard {
    set: Arc<DashSet<String>>,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.remove(&self.key);
    }
}

/// Lightweight descriptor for a YouTube track awaiting download.
/// Used by the discord adapter to pass lookahead info without coupling
/// to voice adapter types.
#[derive(Debug, Clone)]
pub struct PendingYoutubeDownload {
    pub video_id: String,
    pub blob_path: String,
}
