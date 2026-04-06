pub mod enrichment;
pub mod file_ops;
pub mod media_store;
pub mod playlist;
pub mod recommendation;
pub mod repository;
pub mod search;
pub mod user_library;

// Playback ports — still used by voice/discord adapters
pub mod playback_gateway;

pub use enrichment::{AcoustIdPort, CoverArtPort, FingerprintPort, MusicBrainzPort};
pub use file_ops::FileTagWriterPort;
pub use playlist::PlaylistPort;
pub use recommendation::RecommendationPort;
pub use repository::{AlbumRepository, ArtistRepository, TrackRepository};
pub use search::TrackSearchPort;
pub use user_library::UserLibraryPort;
