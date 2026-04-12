use serde::Deserialize;

use domain::VideoMetadata;

/// Deserialization target for yt-dlp `--dump-json` output.
/// Only the fields we need — yt-dlp output has hundreds of fields.
#[derive(Debug, Deserialize)]
pub(crate) struct YtDlpJson {
    pub id: Option<String>,
    /// The original URL or the watch URL.
    pub webpage_url: Option<String>,
    #[allow(unused)]
    #[serde(alias = "url")]
    pub direct_url: Option<String>,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub channel_id: Option<String>,
    /// Duration in seconds (float).
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    /// Music track title (for music videos uploaded by labels).
    pub track: Option<String>,
    /// Music artist name.
    pub artist: Option<String>,
    /// Music album name.
    pub album: Option<String>,
}

impl YtDlpJson {
    pub fn into_video_metadata(self) -> VideoMetadata {
        let video_id = self.id.unwrap_or_default();
        let url = self
            .webpage_url
            .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={video_id}"));

        let duration_ms = self.duration.map(|d| (d * 1000.0) as i64);

        VideoMetadata {
            video_id,
            url,
            title: self.title,
            uploader: self.uploader,
            channel_id: self.channel_id,
            duration_ms,
            thumbnail_url: self.thumbnail,
            track_title: self.track,
            artist: self.artist,
            album: self.album,
        }
    }
}
