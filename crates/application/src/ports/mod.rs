pub mod enrichment;
pub mod file_ops;
pub mod library;
pub mod repository;
pub mod search;

// Keep v1 ports for adapter compatibility during transition
pub mod media_repository;
pub mod media_store;
pub mod playback_gateway;
pub mod settings_repository;

pub use enrichment::{AcoustIdPort, CoverArtPort, FingerprintPort, MusicBrainzPort};
pub use file_ops::FileTagWriterPort;
pub use library::LibraryQueryPort;
pub use repository::{AlbumRepository, ArtistRepository, TrackRepository};
pub use search::TrackSearchPort;
