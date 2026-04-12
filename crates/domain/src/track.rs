use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::EnrichmentStatus;
use crate::analysis::AnalysisStatus;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Track {
    pub id: Uuid,

    // Audio metadata (synchronized to file tags after enrichment)
    pub title: String,
    pub artist_display: Option<String>,
    pub album_id: Option<Uuid>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub duration_ms: Option<i64>,
    pub genres: Option<Vec<String>>,
    pub year: Option<i32>,

    pub bpm: Option<i32>,
    pub isrc: Option<String>,
    pub lyrics: Option<String>,
    pub bitrate: Option<i32>,
    pub sample_rate: Option<i32>,
    pub channels: Option<i32>,
    pub codec: Option<String>,

    // File identity and change detection (no BLAKE3/file_hash)
    pub audio_fingerprint: Option<String>,
    pub file_modified_at: Option<DateTime<Utc>>,
    pub file_size_bytes: Option<i64>,
    /// Relative to MEDIA_ROOT. `None` for YouTube stubs not yet downloaded.
    pub blob_location: Option<String>,

    // ── YouTube / source tracking ─────────────────────────────────────
    /// Track source: "local" or "youtube". Default "local".
    pub source: String,
    pub youtube_video_id: Option<String>,
    pub youtube_channel_id: Option<String>,
    pub youtube_uploader: Option<String>,
    pub youtube_thumbnail_url: Option<String>,

    // Enrichment pipeline state
    pub mbid: Option<String>,
    pub acoustid_id: Option<String>,
    pub enrichment_status: EnrichmentStatus,
    pub enrichment_confidence: Option<f32>,
    pub enrichment_attempts: i32,
    pub enrichment_locked: bool,
    pub enriched_at: Option<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Pass 4: records when enriched tags were written back to the audio file.
    /// NULL = not yet written (or re-enriched since last write).
    pub tags_written_at: Option<DateTime<Utc>>,

    // ── Audio analysis (bliss-audio) ──────────────────────────────────
    /// Analysis pipeline state — mirrors enrichment_status pattern.
    pub analysis_status: AnalysisStatus,
    pub analysis_attempts: i32,
    pub analysis_locked: bool,
    pub analyzed_at: Option<DateTime<Utc>>,
    // NOTE: bliss_vector is NOT stored in the domain Track.
    // It's only accessed via persistence-layer queries (pgvector).

    // NOTE (v3): search_text and search_vector generated columns removed.
    // Full-text search is handled by adapters-search (Tantivy).
}

/// Lightweight projection used by search queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackSummary {
    pub id: Uuid,
    pub title: String,
    pub artist_display: Option<String>,
    pub album_title: Option<String>,
    pub album_id: Option<Uuid>,
    pub duration_ms: Option<i64>,
    pub blob_location: Option<String>,
}
