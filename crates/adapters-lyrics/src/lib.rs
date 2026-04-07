use application::AppError;
use application::ports::enrichment::LyricsProviderPort;
use async_trait::async_trait;
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
};
use nonzero_ext::nonzero;
use reqwest::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;
use tracing::{debug, info, warn};

#[derive(Clone)]
pub struct LyricsAdapter {
    media_root: PathBuf,
    client: Client,
    limiter: Arc<Limiter>,
}

impl LyricsAdapter {
    pub fn new(media_root: impl AsRef<Path>, user_agent: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(user_agent)
            .build()
            .expect("failed to build reqwest client");

        let limiter = Arc::new(RateLimiter::direct(Quota::per_second(nonzero!(2u32))));

        Self {
            media_root: media_root.as_ref().to_path_buf(),
            client,
            limiter,
        }
    }
}

#[derive(Deserialize)]
struct LrcLibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

#[async_trait]
impl LyricsProviderPort for LyricsAdapter {
    async fn fetch_lyrics(
        &self,
        blob_location: &str,
        track_name: &str,
        artist_name: &str,
        album_name: Option<&str>,
        duration_secs: u32,
    ) -> Result<Option<String>, AppError> {
        // 1. Search for local sidecar .lrc file
        let mut audio_path = self.media_root.join(blob_location);
        audio_path.set_extension("lrc");

        if tokio::fs::try_exists(&audio_path).await.unwrap_or(false) {
            debug!(path = %audio_path.display(), "Found local .lrc file");
            match tokio::fs::read_to_string(&audio_path).await {
                Ok(content) => return Ok(Some(content)),
                Err(e) => {
                    warn!(error = %e, path = %audio_path.display(), "Failed to read local .lrc file, falling back to LRCLIB");
                }
            }
        }

        // 2. Query LRCLIB
        self.limiter.until_ready().await;
        debug!(
            track_name,
            artist_name, duration_secs, "Querying LRCLIB API"
        );

        let duration_str = duration_secs.to_string();
        let mut query_params: Vec<(&str, &str)> = vec![
            ("track_name", track_name),
            ("artist_name", artist_name),
            ("duration", &duration_str),
        ];
        if let Some(album) = album_name {
            query_params.push(("album_name", album));
        }

        let url = reqwest::Url::parse_with_params("https://lrclib.net/api/get", &query_params)
            .map_err(|e| AppError::Config {
                field: "lrclib_url",
                message: e.to_string(),
            })?;

        let res = match self.client.get(url).send().await {
            Ok(res) => res,
            Err(e) => {
                warn!(error = %e, "LRCLIB API request failed");
                return Ok(None);
            }
        };

        if res.status() == reqwest::StatusCode::NOT_FOUND {
            debug!("LRCLIB API returned 404 Not Found");
            return Ok(None);
        }

        if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            warn!("LRCLIB API returned 429 Too Many Requests");
            return Err(AppError::LrcLib {
                kind: application::error::LrcLibErrorKind::RateLimited,
                detail: "Rate limit exceeded".to_string(),
            });
        }

        let res = match res.error_for_status() {
            Ok(res) => res,
            Err(e) => {
                warn!(error = %e, "LRCLIB API returned error status");
                return Ok(None);
            }
        };

        let payload = match res.json::<LrcLibResponse>().await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse LRCLIB response");
                return Ok(None);
            }
        };

        let lyrics = payload
            .synced_lyrics
            .filter(|s| !s.is_empty())
            .or_else(|| payload.plain_lyrics.filter(|s| !s.is_empty()));

        if let Some(ref lrc_text) = lyrics {
            if let Some(parent) = audio_path.parent()
                && let Err(e) = tokio::fs::create_dir_all(parent).await
            {
                warn!(error = %e, "Failed to ensure .lrc directory exists");
            }
            match tokio::fs::write(&audio_path, lrc_text).await {
                Ok(()) => {
                    info!(path = %audio_path.display(), "Successfully wrote local .lrc sidecar file");
                }
                Err(e) => {
                    warn!(error = %e, path = %audio_path.display(), "Failed to write local .lrc sidecar file");
                }
            }
        }

        Ok(lyrics)
    }
}
