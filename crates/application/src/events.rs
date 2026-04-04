use uuid::Uuid;

fn new_correlation_id() -> Uuid {
    Uuid::new_v4()
}

/// Emitted by the Fingerprint Worker when a new or changed track is indexed.
/// Received by the Enrichment Orchestrator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrackScanned {
    pub track_id: Uuid,
    pub fingerprint: String,
    pub duration_secs: u32,
    pub blob_location: String,
    #[serde(default = "new_correlation_id")]
    pub correlation_id: Uuid,
}

/// Emitted by the Enrichment Orchestrator to the AcoustID adapter.
#[derive(Debug, Clone)]
pub struct AcoustIdRequest {
    pub track_id: Uuid,
    pub fingerprint: String,
    pub duration_secs: u32,
    /// Carried from claim_for_enrichment — eliminates a DB read in
    /// the `AcoustID` worker (D1 fix).
    pub enrichment_attempts: i32,
    /// Carried through the pipeline so downstream workers don't need
    /// to re-fetch the track row (D2 fix).
    pub blob_location: String,
    pub correlation_id: Uuid,
}

/// Emitted by AcoustID Worker on successful match.
/// Consumed by `MusicBrainz` Worker.
#[derive(Debug, Clone)]
pub struct ToMusicBrainz {
    pub track_id: Uuid,
    pub mbid: String,        // MusicBrainz Recording ID
    pub acoustid_id: String, // AcoustID track ID
    pub confidence: f32,
    pub duration_secs: u32, // carried through for MusicBrainz use
    /// Carried through the pipeline (D2 fix).
    pub blob_location: String,
    /// DESIGN-3: Carried through pipeline — eliminates DB re-fetch in error path.
    pub enrichment_attempts: i32,
    pub correlation_id: Uuid,
}

/// Emitted by MusicBrainz Worker after metadata is written to DB.
/// Consumed by Cover Art Worker.
#[derive(Debug, Clone)]
pub struct ToCoverArt {
    pub track_id: Uuid,
    pub album_id: Option<Uuid>,
    /// MusicBrainz Release ID — used for Cover Art Archive lookup.
    pub release_mbid: String,
    /// Directory of the audio file, relative to MEDIA_ROOT.
    /// None if the file is in MEDIA_ROOT root (Unsorted/).
    pub album_dir: Option<String>,
    /// Relative path to the audio file — used for embedded art fallback.
    pub blob_location: String,
    /// DESIGN-3/DESIGN-4: Carried through pipeline to preserve attempt count.
    pub enrichment_attempts: i32,
    pub correlation_id: Uuid,
}

/// Emitted by the Cover Art Worker after a track reaches 'done'.
/// Consumed by the Tag Writer Worker for file tag synchronization.
#[derive(Debug, Clone)]
pub struct ToTagWriter {
    pub track_id: Uuid,
    /// Relative path to the audio file (relative to MEDIA_ROOT).
    /// Passed through to avoid a DB round trip in the worker.
    pub blob_location: String,
    pub correlation_id: Uuid,
}
