use uuid::Uuid;

/// Emitted by the Fingerprint Worker when a new or changed track is indexed.
/// Received by the Enrichment Orchestrator.
#[derive(Debug, Clone)]
pub struct TrackScanned {
    pub track_id: Uuid,
    pub fingerprint: String,
    pub duration_secs: u32,
}

/// Emitted by the Enrichment Orchestrator to the AcoustID adapter.
#[derive(Debug, Clone)]
pub struct AcoustIdRequest {
    pub track_id: Uuid,
    pub fingerprint: String,
    pub duration_secs: u32,
}
