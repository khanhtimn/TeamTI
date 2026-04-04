pub mod enrichment;
pub mod file_ops;
pub mod library;
pub mod repository;
pub mod search;

// Playback ports — still used by voice/discord adapters
pub mod media_store;
pub mod playback_gateway;

pub use enrichment::{AcoustIdPort, CoverArtPort, FingerprintPort, MusicBrainzPort};
pub use file_ops::FileTagWriterPort;
pub use library::LibraryQueryPort;
pub use repository::{AlbumRepository, ArtistRepository, TrackRepository};
pub use search::TrackSearchPort;
