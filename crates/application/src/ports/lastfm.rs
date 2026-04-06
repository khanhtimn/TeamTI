use async_trait::async_trait;

use crate::AppError;

/// A similar artist returned by Last.fm.
#[derive(Debug, Clone)]
pub struct SimilarArtist {
    pub mbid: String,
    pub name: String,
    pub similarity_score: f32,
}

#[async_trait]
pub trait LastFmPort: Send + Sync {
    /// Fetch artists similar to the given MusicBrainz artist MBID.
    /// Returns an empty Vec if the artist is unknown to Last.fm.
    async fn get_similar_artists(&self, artist_mbid: &str) -> Result<Vec<SimilarArtist>, AppError>;
}
