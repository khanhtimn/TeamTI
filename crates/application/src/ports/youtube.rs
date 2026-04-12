use async_trait::async_trait;
use uuid::Uuid;

use crate::AppError;
use domain::{NewYoutubeDownloadJob, Track, YoutubeDownloadJob};

#[async_trait]
pub trait YoutubeRepository: Send + Sync {
    /// Insert stub Track for a YouTube video (blob_location = NULL).
    /// Idempotent: ON CONFLICT (youtube_video_id) DO NOTHING.
    /// Returns the track_id (either newly created or existing).
    async fn create_youtube_stub(&self, meta: &domain::VideoMetadata) -> Result<Uuid, AppError>;

    /// Insert a batch of YouTube stubs for playlists.
    /// Returns the (video_id, track_id) pairs for all new OR existing stubs.
    async fn create_youtube_stubs_batch(
        &self,
        metas: &[domain::VideoMetadata],
    ) -> Result<Vec<(String, Uuid)>, AppError>;

    async fn find_track_by_video_id(&self, video_id: &str) -> Result<Option<Track>, AppError>;

    /// Update an existing stub with full metadata (called by C3 repair logic).
    async fn update_youtube_stub_metadata(
        &self,
        video_id: &str,
        meta: &domain::VideoMetadata,
    ) -> Result<(), AppError>;

    /// Look up an existing download job by video_id.
    async fn get_download_job(
        &self,
        video_id: &str,
    ) -> Result<Option<YoutubeDownloadJob>, AppError>;

    /// Insert a new download job (ON CONFLICT DO NOTHING — idempotent).
    async fn upsert_download_job(&self, job: &NewYoutubeDownloadJob) -> Result<(), AppError>;

    /// Mark job as 'downloading', set started_at.
    /// Returns true if the job was successfully claimed, false if already claimed.
    async fn lock_download_job(&self, video_id: &str) -> Result<bool, AppError>;

    /// Mark job as 'done', set blob_location on associated track.
    async fn complete_download_job(
        &self,
        video_id: &str,
        blob_location: &str,
    ) -> Result<(), AppError>;

    /// Mark job as 'failed', increment attempts, record error.
    async fn fail_download_job(&self, video_id: &str, error: &str) -> Result<(), AppError>;

    /// Permanently fail job + optionally delete the stub track.
    /// Deletes the stub only if it has zero listen_events.
    async fn permanently_fail_download_job(&self, video_id: &str) -> Result<(), AppError>;

    /// On startup: reset stuck 'downloading' jobs older than threshold.
    async fn unlock_stale_download_jobs(
        &self,
        older_than: std::time::Duration,
    ) -> Result<u64, AppError>;
}
