mod response;

use async_trait::async_trait;
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
};
use nonzero_ext::nonzero;
use reqwest::Client;
use std::sync::Arc;

use application::AppError;
use application::error::AcoustIdErrorKind;
use application::ports::AcoustIdPort;
use application::ports::enrichment::{AcoustIdMatch, AudioFingerprint};

use response::AcoustIdResponse;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct AcoustIdAdapter {
    client: Client,
    limiter: Arc<Limiter>,
    api_key: String,
}

impl AcoustIdAdapter {
    #[must_use]
    pub fn new(api_key: String) -> Self {
        let limiter = Arc::new(RateLimiter::direct(Quota::per_second(nonzero!(1u32))));
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build AcoustID HTTP client");
        Self {
            client,
            limiter,
            api_key,
        }
    }
}

#[async_trait]
impl AcoustIdPort for AcoustIdAdapter {
    async fn lookup(&self, fp: &AudioFingerprint) -> Result<Option<AcoustIdMatch>, AppError> {
        // Block until the rate limiter permits the next request.
        // until_ready() yields to the tokio scheduler — no busy-wait.
        self.limiter.until_ready().await;

        let duration_secs_str = (fp.duration_ms / 1000).to_string();

        let resp = self
            .client
            .post("https://api.acoustid.org/v2/lookup")
            .form(&[
                ("client", self.api_key.as_str()),
                ("fingerprint", fp.fingerprint.as_str()),
                ("duration", &duration_secs_str),
                ("meta", "recordings compress"),
            ])
            .send()
            .await
            .map_err(|e| AppError::AcoustId {
                kind: AcoustIdErrorKind::HttpError,
                detail: format!("AcoustID request failed: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let kind = match status {
                reqwest::StatusCode::TOO_MANY_REQUESTS => AcoustIdErrorKind::RateLimited,
                s if s.is_server_error() => AcoustIdErrorKind::ServiceUnavailable,
                _ => AcoustIdErrorKind::HttpError,
            };

            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".into());

            return Err(AppError::AcoustId {
                kind,
                detail: format!("AcoustID returned HTTP {status}: {body_text}"),
            });
        }

        let body_text = resp.text().await.map_err(|e| AppError::AcoustId {
            kind: AcoustIdErrorKind::InvalidResponse,
            detail: format!("failed to read AcoustID response body: {e}"),
        })?;

        let body: AcoustIdResponse =
            serde_json::from_str(&body_text).map_err(|e| AppError::AcoustId {
                kind: AcoustIdErrorKind::InvalidResponse,
                detail: format!("AcoustID JSON parse error: {e} — body: {body_text}",),
            })?;

        if body.status != "ok" {
            return Err(AppError::AcoustId {
                kind: AcoustIdErrorKind::InvalidResponse,
                detail: format!("AcoustID status: {}", body.status),
            });
        }

        // Select the best result: highest score, must have at least one recording.
        let best = body
            .results
            .into_iter()
            .filter(|r| !r.recordings.is_empty())
            .max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        // B3 fix: among recordings in the best result, prefer the one whose
        // duration is closest to our decoded duration. AcoustID's recording
        // list is not ranked — the first entry may be a live or alternate take.
        // Compare in ms-space: AcoustID returns seconds (f64), we have ms (i64).
        Ok(best.map(|r| {
            let best_rec = r
                .recordings
                .iter()
                .min_by_key(|rec| {
                    rec.duration.map_or(i64::MAX, |d_secs| {
                        ((d_secs * 1000.0) as i64 - fp.duration_ms).abs()
                    })
                })
                .unwrap_or(&r.recordings[0]);

            AcoustIdMatch {
                recording_mbid: best_rec.id.clone(),
                score: r.score,
                acoustid_id: r.id,
            }
        }))
    }
}
