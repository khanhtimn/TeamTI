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

    /// Update lyrics for a track.
    async fn update_lyrics(&self, track_id: Uuid, lyrics: &str) -> Result<(), AppError>;

    /// Fetch all track credits (composers and lyricists) in a single query.
    async fn get_credits(&self, track_id: Uuid) -> Result<TrackCredits, AppError>;

    // ── Analysis worker methods ───────────────────────────────────────

    /// Atomically claim tracks for analysis (FOR UPDATE SKIP LOCKED).
    async fn claim_for_analysis(&self, limit: i64) -> Result<Vec<Track>, AppError>;

    /// Unlock stuck analysis rows from previous crashed sessions.
    async fn unlock_stale_analysis_rows(
        &self,
        older_than: std::time::Duration,
    ) -> Result<u64, AppError>;

    /// Mark a track's analysis as done with the computed bliss vector.
    async fn update_analysis_done(
        &self,
        track_id: Uuid,
        bliss_vector: &[f32],
    ) -> Result<(), AppError>;

    /// Mark a track's analysis as failed and increment attempts.
    async fn update_analysis_failed(&self, track_id: Uuid) -> Result<(), AppError>;

    /// Startup watchdog: reset stale 'processing' analysis rows to 'pending'.
    async fn reset_stale_analyzing(&self) -> Result<u64, AppError>;

    // ── Last.fm similarity cache ──────────────────────────────────────

    /// Check which artist MBIDs are already cached in similar_artists
    async fn get_cached_similar_artists(&self, mbids: &[String]) -> Result<Vec<String>, AppError>;

    /// Bulk upsert similar artist relationships from Last.fm.
    async fn upsert_similar_artists(
        &self,
        source_mbid: &str,
        similar: &[crate::ports::lastfm::SimilarArtist],
    ) -> Result<(), AppError>;
}

/// Consolidated track credits.
#[derive(Debug, Clone)]
pub struct TrackCredits {
    pub composers: Vec<String>,
    pub lyricists: Vec<String>,
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

#[async_trait]
pub trait YoutubeSearchRepository: Send + Sync {
    /// Upsert a search result into youtube_search_cache.
    /// ON CONFLICT (video_id) DO UPDATE SET last_seen_at = now().
    async fn upsert_search_result(
        &self,
        query: &str,
        result: &domain::youtube::VideoMetadata,
    ) -> Result<(), AppError>;

    /// Look up a single search cache entry by video_id.
    async fn find_search_cache_by_video_id(
        &self,
        video_id: &str,
    ) -> Result<Option<domain::youtube::YoutubeSearchCacheRow>, AppError>;

    /// Given a list of video_ids, return those that already exist in tracks.
    /// Used to avoid indexing duplicates in Tantivy.
    async fn find_existing_video_ids(
        &self,
        video_ids: &[String],
    ) -> Result<std::collections::HashSet<String>, AppError>;

    /// Update youtube_search_cache.track_id when a search stub is downloaded.
    async fn link_search_cache_to_track(
        &self,
        video_id: &str,
        track_id: Uuid,
    ) -> Result<(), AppError>;
}
