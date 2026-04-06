//! Last.fm JSON API response types.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SimilarArtistsResponse {
    pub similarartists: SimilarArtists,
}

#[derive(Debug, Deserialize)]
pub struct SimilarArtists {
    #[serde(default)]
    pub artist: Vec<SimilarArtistEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SimilarArtistEntry {
    pub name: String,
    pub mbid: Option<String>,
    /// Called "match" in the JSON, but that's a Rust keyword.
    #[serde(rename = "match")]
    pub matchfield: String,
}
