use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::EnrichmentStatus;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Track {
    pub id: Uuid,

    // Audio metadata (synchronized to file tags after enrichment)
    pub title: String,
    pub artist_display: Option<String>,
    pub album_id: Option<Uuid>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub duration_ms: Option<i32>,
    pub genre: Option<String>,
    pub year: Option<i32>,

    // File identity and change detection (no BLAKE3/file_hash)
    pub audio_fingerprint: Option<String>,
    pub file_modified_at: Option<DateTime<Utc>>,
    pub file_size_bytes: Option<i64>,
    /// Always relative to MEDIA_ROOT
    pub blob_location: String,

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
    // NOTE: search_text and search_vector are generated columns.
    // They are NOT included in INSERT/UPDATE statements.
    // They are read-only and excluded from the Track struct by default.
    // Use TrackSummary for queries that need them.
}

/// Lightweight projection used by search queries.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TrackSummary {
    pub id: Uuid,
    pub title: String,
    pub artist_display: Option<String>,
    pub album_id: Option<Uuid>,
    pub duration_ms: Option<i32>,
    pub blob_location: String,
}
