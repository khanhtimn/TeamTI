pub mod acoustid_worker;
pub mod cover_art_worker;
pub mod enrichment_orchestrator;
pub mod error;
pub mod events;
pub mod lyrics_worker;
pub mod musicbrainz_worker;
pub mod ports;
pub mod tag_writer_worker;

// Keep v1 services module for adapter compatibility during transition
pub mod dto;
pub mod services;

pub use acoustid_worker::AcoustIdWorker;
pub use cover_art_worker::CoverArtWorker;
pub use enrichment_orchestrator::EnrichmentOrchestrator;
pub use error::{AppError, PlaylistErrorKind, SearchErrorKind};
pub use events::{AcoustIdRequest, ToLyrics, TrackScanned};
pub use lyrics_worker::LyricsWorker;
pub use musicbrainz_worker::MusicBrainzWorker;
pub use tag_writer_worker::TagWriterWorker;

/// A listen event is "completed" when the user listened to at least
/// this fraction of the track duration.
pub const LISTEN_COMPLETION_THRESHOLD: f32 = 0.80;

/// Number of tracks remaining in the queue at which radio mode triggers a refill.
pub const RADIO_REFILL_THRESHOLD: usize = 2;

/// Number of tracks added per radio refill batch.
pub const RADIO_BATCH_SIZE: usize = 5;

/// Maximum number of collaborators per playlist.
pub const PLAYLIST_COLLABORATOR_LIMIT: usize = 10;
