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
pub use error::AppError;
pub use events::{AcoustIdRequest, ToLyrics, TrackScanned};
pub use lyrics_worker::LyricsWorker;
pub use musicbrainz_worker::MusicBrainzWorker;
pub use tag_writer_worker::TagWriterWorker;
