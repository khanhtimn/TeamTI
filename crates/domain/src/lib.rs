pub mod album;
pub mod artist;
pub mod enrichment;
pub mod track;
pub mod user_library;

// Re-exported v1 types kept for adapter compatibility during transition
pub mod error;
pub mod guild;
pub mod media;
pub mod playback;

pub use album::Album;
pub use artist::{AlbumArtist, Artist, ArtistRole, TrackArtist};
pub use enrichment::EnrichmentStatus;
pub use track::Track;
pub use user_library::{Favorite, ListenEvent, Playlist, PlaylistItem};
