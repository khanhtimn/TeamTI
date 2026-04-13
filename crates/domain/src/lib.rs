pub mod album;
pub mod analysis;
pub mod artist;
pub mod autocomplete;
pub mod enrichment;
pub mod search;
pub mod track;
pub mod user_library;
pub mod youtube;

// Re-exported v1 types kept for adapter compatibility during transition
pub mod error;
pub mod guild;
pub mod media;
pub mod playback;

pub use album::Album;
pub use analysis::{AnalysisStatus, MoodWeight};
pub use artist::{AlbumArtist, Artist, ArtistRole, TrackArtist};
pub use enrichment::EnrichmentStatus;
pub use track::Track;
pub use user_library::{
    Favorite, FavouritesPage, ListenEvent, Playlist, PlaylistItem, PlaylistPage, PlaylistSummary,
    PlaylistVisibility,
};
pub use youtube::{NewYoutubeDownloadJob, VideoMetadata, YoutubeDownloadJob};
