use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::AppError;
use domain::{Album, AlbumArtist, Artist, Track, TrackArtist};

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait TrackRepository: Send + Sync {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Track>, AppError>;

    async fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Track>, AppError>;

    async fn find_by_blob_location(&self, location: &str) -> Result<Option<Track>, AppError>;

    async fn find_many_by_blob_location(
        &self,
        locations: &[String],
    ) -> Result<std::collections::HashMap<String, Track>, AppError>;

    async fn insert(&self, track: &Track) -> Result<(Track, bool), AppError>;

    async fn update_file_identity(
        &self,
        id: Uuid,
        file_modified_at: DateTime<Utc>,
        file_size_bytes: i64,
        blob_location: &str,
    ) -> Result<(), AppError>;

    async fn update_fingerprint(&self, id: Uuid, fingerprint: &str) -> Result<(), AppError>;

    async fn update_enrichment_status(
        &self,
        id: Uuid,
        status: &domain::EnrichmentStatus,
        attempts: i32,
        enriched_at: Option<DateTime<Utc>>,
    ) -> Result<(), AppError>;

    async fn update_enriched_metadata(
        &self,
        id: Uuid,
        title: &str,
        artist_display: &str,
        album_id: Option<Uuid>,
        genre: Option<&str>,
        year: Option<i32>,
        mbid: Option<&str>,
        acoustid_id: Option<&str>,
        confidence: Option<f32>,
    ) -> Result<(), AppError>;

    /// Used by enrichment orchestrator — FOR UPDATE SKIP LOCKED
    async fn claim_for_enrichment(
        &self,
        failed_retry_limit: u32,
        unmatched_retry_limit: u32,
        limit: i64,
    ) -> Result<Vec<Track>, AppError>;

    /// Startup watchdog: reset stale 'enriching' rows to 'pending'
    async fn reset_stale_enriching(&self) -> Result<u64, AppError>;

    /// /rescan --force: reset exhausted + low_confidence to pending
    async fn force_rescan(&self) -> Result<u64, AppError>;

    async fn mark_file_missing(&self, blob_location: &str) -> Result<(), AppError>;
}

#[async_trait]
pub trait ArtistRepository: Send + Sync {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Artist>, AppError>;

    async fn upsert(&self, artist: &Artist) -> Result<Artist, AppError>;

    async fn upsert_track_artist(&self, ta: &TrackArtist) -> Result<(), AppError>;

    async fn upsert_album_artist(&self, aa: &AlbumArtist) -> Result<(), AppError>;
}

#[async_trait]
pub trait AlbumRepository: Send + Sync {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Album>, AppError>;

    async fn upsert(&self, album: &Album) -> Result<Album, AppError>;

    async fn update_cover_art_path(&self, id: Uuid, path: &str) -> Result<(), AppError>;
}
