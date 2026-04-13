pub mod audio_analysis;
pub mod enrichment;
pub mod file_ops;
pub mod lastfm;
pub mod media_store;
pub mod playlist;
pub mod recommendation;
pub mod repository;
pub mod search;
pub mod user_library;
pub mod youtube;
pub mod ytdlp;

// Playback ports — still used by voice/discord adapters
pub mod playback_gateway;

pub use audio_analysis::AudioAnalysisPort;
pub use enrichment::{AcoustIdPort, CoverArtPort, FingerprintPort, MusicBrainzPort};
pub use file_ops::FileTagWriterPort;
pub use lastfm::LastFmPort;
pub use playlist::PlaylistPort;
pub use recommendation::RecommendationPort;
pub use repository::{AlbumRepository, ArtistRepository, TrackRepository};
pub use search::MusicSearchPort;
pub use user_library::UserLibraryPort;
pub use youtube::YoutubeRepository;
pub use ytdlp::YtDlpPort;
