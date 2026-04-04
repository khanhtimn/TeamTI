use async_trait::async_trait;
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use reqwest::Client;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

use application::AppError;
use application::error::CoverArtErrorKind;
use application::ports::CoverArtPort;

pub struct CoverArtAdapter {
    client: Client,
    semaphore: Arc<Semaphore>,
}

impl CoverArtAdapter {
    pub fn new(concurrency: usize) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            // Follow redirects — CAA returns 307 before the image URL.
            .redirect(reqwest::redirect::Policy::limited(5))
            // E3: send descriptive User-Agent to CAA for good practice.
            .user_agent("TeamTI/2.0 (github.com/khanhtimn/teamti)")
            .build()
            .expect("failed to build cover art HTTP client");
        Self {
            client,
            semaphore: Arc::new(Semaphore::new(concurrency)),
        }
    }
}

#[async_trait]
impl CoverArtPort for CoverArtAdapter {
    async fn fetch_front(&self, release_mbid: &str) -> Result<Option<bytes::Bytes>, AppError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| AppError::CoverArt {
                kind: CoverArtErrorKind::ServiceUnavailable,
                detail: "cover art semaphore closed".into(),
            })?;

        let url = format!("https://coverartarchive.org/release/{release_mbid}/front-500");

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::CoverArt {
                kind: CoverArtErrorKind::HttpError,
                detail: format!("CAA request failed: {e}"),
            })?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None); // No cover art available — not an error
        }
        if !resp.status().is_success() {
            let kind = match resp.status() {
                s if s.is_server_error() => CoverArtErrorKind::ServiceUnavailable,
                _ => CoverArtErrorKind::HttpError,
            };
            return Err(AppError::CoverArt {
                kind,
                detail: format!("CAA HTTP {}", resp.status()),
            });
        }

        let bytes = resp.bytes().await.map_err(|e| AppError::CoverArt {
            kind: CoverArtErrorKind::HttpError,
            detail: format!("CAA body read error: {e}"),
        })?;

        Ok(Some(bytes))
    }

    async fn extract_from_tags(&self, path: &Path) -> Result<Option<bytes::Bytes>, AppError> {
        // C1: SMB_READ_SEMAPHORE is intentionally NOT acquired here.
        // Rationale: cover art extraction reads only the tag header (typically
        // < 256 KB, often cached), not the full audio stream. The fingerprint
        // decode (which reads up to 120s of PCM) dominates SMB bandwidth.
        // The two workers (Fingerprint and Cover Art) rarely overlap at scale.
        // If NAS contention is observed, promote this to a semaphore-guarded read.
        // See: master doc §Invariant 4 and §adapters-cover-art.
        let path_for_err = path.to_owned();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let tagged = lofty::read_from_path(&path)?;
            let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

            // C3 fix: use an explicit priority list instead of falling back to
            // PictureType::Other, which includes artist photos, back covers,
            // band logos, and lyric sheets — not just cover art.
            const PREFERRED: &[PictureType] = &[
                PictureType::CoverFront,
                PictureType::Media,
                PictureType::Leaflet,
                PictureType::Illustration,
            ];

            let art = tag.and_then(|t| {
                PREFERRED
                    .iter()
                    .find_map(|ptype| t.pictures().iter().find(|p| p.pic_type() == *ptype))
                    .map(|p| bytes::Bytes::copy_from_slice(p.data()))
            });

            Ok::<_, lofty::error::LoftyError>(art)
        })
        .await
        .map_err(|e| AppError::Io {
            path: Some(path_for_err.clone()),
            source: std::io::Error::other(e),
        })?
        .map_err(|e| AppError::TagRead {
            path: path_for_err,
            source: Box::new(std::io::Error::other(format!("lofty tag read error: {e}"))),
        })
    }
}
