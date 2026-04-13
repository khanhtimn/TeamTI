mod metadata;
mod sanitize;

pub use sanitize::{canonical_youtube_url, extract_youtube_playlist_id, extract_youtube_video_id};

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{debug, warn};

use application::AppError;
use application::error::YouTubeErrorKind;
use application::ports::ytdlp::YtDlpPort;
use domain::VideoMetadata;

use metadata::YtDlpJson;

/// yt-dlp subprocess adapter implementing `YtDlpPort`.
#[derive(Clone)]
pub struct YtDlpAdapter {
    binary: String,
    cookies_file: Option<String>,
    ffmpeg_location: Option<String>,
}

impl YtDlpAdapter {
    #[must_use]
    pub fn new(
        binary: String,
        cookies_file: Option<String>,
        ffmpeg_location: Option<String>,
    ) -> Self {
        Self {
            binary,
            cookies_file,
            ffmpeg_location,
        }
    }

    fn base_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        if let Some(ref loc) = self.ffmpeg_location {
            cmd.arg("--ffmpeg-location").arg(loc);
        }
        cmd.kill_on_drop(true);
        cmd.arg("--no-check-certificates").arg("--no-playlist");
        if let Some(ref cookies) = self.cookies_file {
            cmd.arg("--cookies").arg(cookies);
        }
        cmd
    }

    fn playlist_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.kill_on_drop(true);
        cmd.arg("--no-check-certificates");
        if let Some(ref cookies) = self.cookies_file {
            cmd.arg("--cookies").arg(cookies);
        }
        cmd
    }

    async fn run_json(&self, cmd: &mut Command, timeout: Duration) -> Result<String, AppError> {
        let output = tokio::time::timeout(
            timeout,
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output(),
        )
        .await
        .map_err(|_| AppError::YouTube {
            kind: YouTubeErrorKind::SubprocessFailed,
            detail: format!(
                "yt-dlp metadata fetch timed out after {}s",
                timeout.as_secs()
            ),
        })?
        .map_err(|e| AppError::YouTube {
            kind: YouTubeErrorKind::SubprocessFailed,
            detail: format!("failed to spawn yt-dlp: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Detect "Video unavailable" pattern
            if stderr.contains("Video unavailable")
                || stderr.contains("Private video")
                || stderr.contains("This video is not available")
            {
                return Err(AppError::YouTube {
                    kind: YouTubeErrorKind::VideoUnavailable,
                    detail: stderr.to_string(),
                });
            }
            return Err(AppError::YouTube {
                kind: YouTubeErrorKind::SubprocessFailed,
                detail: format!("yt-dlp exited with {}: {}", output.status, stderr),
            });
        }

        String::from_utf8(output.stdout).map_err(|e| AppError::YouTube {
            kind: YouTubeErrorKind::MetadataParse,
            detail: format!("yt-dlp output is not valid UTF-8: {e}"),
        })
    }
}

#[async_trait]
impl YtDlpPort for YtDlpAdapter {
    async fn fetch_video_metadata(&self, url: &str) -> Result<VideoMetadata, AppError> {
        let mut cmd = self.base_cmd();
        cmd.arg("--dump-json").arg("--skip-download").arg(url);

        let json_str = self.run_json(&mut cmd, Duration::from_secs(30)).await?;
        let parsed: YtDlpJson = serde_json::from_str(&json_str).map_err(|e| AppError::YouTube {
            kind: YouTubeErrorKind::MetadataParse,
            detail: format!("failed to parse yt-dlp JSON: {e}"),
        })?;

        Ok(parsed.into_video_metadata())
    }

    async fn fetch_playlist_metadata(&self, url: &str) -> Result<Vec<VideoMetadata>, AppError> {
        let mut cmd = self.playlist_cmd();
        cmd.arg("--flat-playlist")
            .arg("--dump-json")
            .arg("--skip-download")
            .arg(url);

        // F4: playlists can have 100+ entries; 30s is too short
        let json_str = self.run_json(&mut cmd, Duration::from_secs(120)).await?;

        let mut results = Vec::new();
        for line in json_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<YtDlpJson>(line) {
                Ok(entry) => results.push(entry.into_video_metadata()),
                Err(e) => {
                    warn!("skipping unparseable playlist entry: {e}");
                }
            }
        }

        Ok(results)
    }

    async fn search_top_result(&self, query: &str) -> Result<Option<VideoMetadata>, AppError> {
        let mut cmd = self.base_cmd();
        // F7: ytsearch1: prefix already instructs yt-dlp to search;
        // --default-search is only needed for bare text without a prefix.
        let search_query = format!("ytsearch1:{query}");
        cmd.arg("--dump-json")
            .arg("--skip-download")
            .arg(&search_query);

        let json_str = self.run_json(&mut cmd, Duration::from_secs(30)).await?;
        if json_str.trim().is_empty() {
            return Ok(None);
        }

        let parsed: YtDlpJson =
            serde_json::from_str(json_str.trim()).map_err(|e| AppError::YouTube {
                kind: YouTubeErrorKind::MetadataParse,
                detail: format!("failed to parse yt-dlp search JSON: {e}"),
            })?;

        Ok(Some(parsed.into_video_metadata()))
    }

    async fn search_top_n(&self, query: &str, n: usize) -> Result<Vec<VideoMetadata>, AppError> {
        let mut cmd = self.base_cmd();
        let search_query = format!("ytsearch{n}:{query}");
        cmd.arg("--dump-json")
            .arg("--skip-download")
            .arg(&search_query);

        let json_str = self.run_json(&mut cmd, Duration::from_secs(60)).await?;
        let mut results = Vec::new();

        for line in json_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<YtDlpJson>(line) {
                Ok(entry) => results.push(entry.into_video_metadata()),
                Err(e) => {
                    warn!("skipping unparseable search result entry: {e}");
                }
            }
        }

        Ok(results)
    }

    async fn download_audio(&self, url: &str, output_path: &Path) -> Result<(), AppError> {
        let dir = output_path.parent().unwrap_or_else(|| Path::new("."));

        // F12: Use tempfile for atomic creation (O_EXCL) and RAII cleanup.
        // into_temp_path() closes the FD so yt-dlp gets exclusive write access.
        let temp_path = tempfile::Builder::new()
            .prefix(".ytdl.")
            .suffix(".m4a")
            .tempfile_in(dir)
            .map_err(|e| AppError::YouTube {
                kind: YouTubeErrorKind::DownloadFailed,
                detail: format!("failed to create temp file: {e}"),
            })?
            .into_temp_path();

        let mut cmd = Command::new(&self.binary);
        if let Some(ref loc) = self.ffmpeg_location {
            cmd.arg("--ffmpeg-location").arg(loc);
        }
        cmd.kill_on_drop(true);
        cmd.arg("--no-check-certificates");
        if let Some(ref cookies) = self.cookies_file {
            cmd.arg("--cookies").arg(cookies);
        }
        cmd.arg("--no-playlist")
            .arg("-x")
            .arg("--audio-format")
            .arg("m4a")
            .arg("--audio-quality")
            .arg("0")
            .arg("-o")
            .arg(temp_path.to_string_lossy().as_ref())
            .arg(url);

        debug!(url, output = %output_path.display(), "starting yt-dlp download");

        let output = tokio::time::timeout(
            Duration::from_secs(300), // 5 minute timeout for full download
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output(),
        )
        .await
        .map_err(|_| AppError::YouTube {
            kind: YouTubeErrorKind::DownloadFailed,
            detail: "yt-dlp download timed out".to_string(),
        })?
        .map_err(|e| AppError::YouTube {
            kind: YouTubeErrorKind::DownloadFailed,
            detail: format!("failed to spawn yt-dlp download: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Detect "Video unavailable" / private video pattern
            if stderr.contains("Video unavailable")
                || stderr.contains("Private video")
                || stderr.contains("This video is not available")
                || stderr.contains("Sign in to confirm your age")
            {
                return Err(AppError::YouTube {
                    kind: YouTubeErrorKind::VideoUnavailable,
                    detail: stderr.to_string(),
                });
            }
            // temp_path drops here → auto-deletes partial download
            return Err(AppError::YouTube {
                kind: YouTubeErrorKind::DownloadFailed,
                detail: format!("yt-dlp download failed ({}): {}", output.status, stderr),
            });
        }

        // Atomic rename: tempfile::TempPath::persist handles cross-device detection.
        temp_path
            .persist(output_path)
            .map_err(|e| AppError::YouTube {
                kind: YouTubeErrorKind::DownloadFailed,
                detail: format!("failed to persist download: {e}"),
            })?;

        Ok(())
    }

    fn compute_blob_path(&self, uploader: &str, title: &str, video_id: &str) -> String {
        youtube_blob_path(uploader, title, video_id)
    }
}

/// Construct a relative blob path for a YouTube download.
/// `$MEDIA_ROOT/youtube/{uploader}/{title}_{video_id}.m4a`
#[must_use]
pub fn youtube_blob_path(uploader: &str, title: &str, video_id: &str) -> String {
    let safe_uploader = sanitize::sanitize_component(uploader);
    let safe_title = sanitize::sanitize_component(title);
    format!("youtube/{safe_uploader}/{safe_title}_{video_id}.m4a")
}
