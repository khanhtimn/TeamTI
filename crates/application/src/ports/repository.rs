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

    /// Persist `AcoustID` match immediately — crash-safe durability at the
    /// `AcoustID` → `MusicBrainz` stage boundary (A1 fix).
    async fn update_acoustid_match(
        &self,
        id: Uuid,
        acoustid_id: &str,
        confidence: f32,
    ) -> Result<(), AppError>;

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
        genres: Option<Vec<String>>,
        year: Option<i32>,
        mbid: Option<&str>,
        acoustid_id: Option<&str>,
        confidence: Option<f32>,
        isrc: Option<&str>,
    ) -> Result<(), AppError>;

    /// Atomically claim a single track for enrichment (reactive path).
    /// Returns None if the track doesn't exist or isn't in a claimable state,
    /// preventing duplicate enrichment with the proactive poll path.
    async fn claim_single(&self, id: Uuid) -> Result<Option<Track>, AppError>;

    /// Used by enrichment orchestrator — FOR UPDATE SKIP LOCKED
    async fn claim_for_enrichment(
        &self,
        failed_retry_limit: i32,
        unmatched_retry_limit: i32,
        limit: i64,
    ) -> Result<Vec<Track>, AppError>;

    /// Startup watchdog: reset stale 'enriching' rows to 'pending'
    async fn reset_stale_enriching(&self) -> Result<u64, AppError>;

    /// /rescan --force: reset `exhausted` + `low_confidence` to `pending`
    async fn force_rescan(&self) -> Result<u64, AppError>;

    async fn mark_file_missing(&self, blob_location: &str) -> Result<(), AppError>;

    /// Called after successful atomic tag writeback.
    /// Updates file identity fields and sets `tags_written_at` = `now()`.
    /// A4 fix: includes AND `enrichment_status` = 'done' safety guard.
    async fn update_file_tags_written(
        &self,
        id: Uuid,
        new_mtime: DateTime<Utc>,
        new_size_bytes: i64,
    ) -> Result<(), AppError>;

    /// Startup poller query: find tracks that are 'done' but haven't had
    /// their file tags written yet.
    async fn find_tags_unwritten(&self, limit: i64) -> Result<Vec<Track>, AppError>;
}

#[async_trait]
pub trait ArtistRepository: Send + Sync {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Artist>, AppError>;

    async fn find_by_track_id(
        &self,
        track_id: Uuid,
    ) -> Result<Vec<(TrackArtist, Artist)>, AppError>;

    async fn upsert(&self, artist: &Artist) -> Result<Artist, AppError>;

    async fn upsert_track_artist(&self, ta: &TrackArtist) -> Result<(), AppError>;

    async fn upsert_album_artist(&self, aa: &AlbumArtist) -> Result<(), AppError>;
}

#[async_trait]
pub trait AlbumRepository: Send + Sync {
    async fn find_by_mbid(&self, mbid: &str) -> Result<Option<Album>, AppError>;

    async fn upsert(&self, album: &Album) -> Result<Album, AppError>;

    async fn update_cover_art_path(&self, id: Uuid, path: &str) -> Result<(), AppError>;

    /// B3 fix: find album by ID for tag writeback.
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Album>, AppError>;
}
