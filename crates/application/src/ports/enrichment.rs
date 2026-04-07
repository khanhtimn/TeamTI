use crate::AppError;
use async_trait::async_trait;
use std::path::Path;

/// Raw tags read from a file before enrichment.
#[derive(Debug, Clone)]
pub struct RawFileTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub genres: Option<Vec<String>>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration_ms: Option<i64>,
    pub bpm: Option<i32>,
    pub isrc: Option<String>,
    pub composer: Option<String>,
    pub lyricist: Option<String>,
    pub lyrics: Option<String>,
    // Audio Properties
    pub bitrate: Option<i32>,
    pub sample_rate: Option<i32>,
    pub channels: Option<i32>,
    pub codec: Option<String>,
}

/// Chromaprint fingerprint result.
#[derive(Debug, Clone)]
pub struct AudioFingerprint {
    pub fingerprint: String, // base64-encoded Chromaprint string
    pub duration_ms: i64,
}

/// Best match returned by AcoustID lookup.
#[derive(Debug, Clone)]
pub struct AcoustIdMatch {
    pub recording_mbid: String,
    pub score: f32,
    pub acoustid_id: String,
}

/// Recording data returned by MusicBrainz.
#[derive(Debug, Clone)]
pub struct MbRecording {
    pub title: String,
    pub artist_credits: Vec<MbArtistCredit>,
    pub release_mbid: String,
    pub release_title: String,
    pub release_year: Option<i32>,
    pub release_date: Option<chrono::NaiveDate>,
    pub genres: Vec<String>,
    pub record_label: Option<String>,
    pub barcode: Option<String>,
    pub isrc: Option<String>,
    pub work_mbid: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MbArtistCredit {
    pub artist_mbid: String,
    pub name: String,
    pub sort_name: String,
    pub join_phrase: Option<String>, // " feat. ", " & ", etc.
}

/// Credits extracted from a Work entity (composer/lyricist).
#[derive(Debug, Clone)]
pub struct MbWorkCredits {
    pub composers: Vec<MbArtistCredit>,
    pub lyricists: Vec<MbArtistCredit>,
}

#[async_trait]
pub trait FingerprintPort: Send + Sync {
    async fn compute(&self, path: &Path) -> Result<(AudioFingerprint, RawFileTags), AppError>;
}

#[async_trait]
pub trait AcoustIdPort: Send + Sync {
    async fn lookup(&self, fp: &AudioFingerprint) -> Result<Option<AcoustIdMatch>, AppError>;
}

#[async_trait]
pub trait MusicBrainzPort: Send + Sync {
    async fn fetch_recording(&self, mbid: &str) -> Result<MbRecording, AppError>;
    /// Fetch composer/lyricist credits from a Work entity.
    async fn fetch_work_credits(&self, work_mbid: &str) -> Result<MbWorkCredits, AppError>;
    /// Fetch label info from a Release entity.
    /// Returns the first label name, or None if no label info.
    async fn fetch_release_label(&self, release_mbid: &str) -> Result<Option<String>, AppError>;
}

#[async_trait]
pub trait CoverArtPort: Send + Sync {
    /// Returns raw image bytes if found, None otherwise.
    async fn fetch_front(&self, release_mbid: &str) -> Result<Option<bytes::Bytes>, AppError>;

    /// Extract embedded art from file tags. None if no embedded art.
    async fn extract_from_tags(
        &self,
        path: &std::path::Path,
    ) -> Result<Option<bytes::Bytes>, AppError>;
}

#[async_trait]
pub trait LyricsProviderPort: Send + Sync {
    /// Search for a localized sidecar .lrc file or query LRCLIB utilizing the track metadata.
    async fn fetch_lyrics(
        &self,
        blob_location: &str,
        track_name: &str,
        artist_name: &str,
        album_name: Option<&str>,
        duration_ms: i64,
    ) -> Result<Option<String>, AppError>;
}
