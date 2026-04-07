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
use application::error::LastFmErrorKind;
use application::ports::lastfm::{LastFmPort, SimilarArtist};

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct LastFmAdapter {
    client: Client,
    limiter: Arc<Limiter>,
    api_key: String,
}

impl LastFmAdapter {
    #[must_use]
    pub fn new(api_key: String) -> Self {
        let limiter = Arc::new(RateLimiter::direct(
            // 4 req/sec — generous for Last.fm but safe
            Quota::per_second(nonzero!(4u32)),
        ));
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build Last.fm HTTP client");
        Self {
            client,
            limiter,
            api_key,
        }
    }
}

#[async_trait]
impl LastFmPort for LastFmAdapter {
    async fn get_similar_artists(&self, artist_mbid: &str) -> Result<Vec<SimilarArtist>, AppError> {
        self.limiter.until_ready().await;

        let url = format!(
            "https://ws.audioscrobbler.com/2.0/?method=artist.getsimilar&mbid={}&api_key={}&format=json&limit=20",
            artist_mbid, self.api_key
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::LastFm {
                kind: LastFmErrorKind::ApiError,
                detail: format!("Last.fm request failed: {e}"),
            })?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !resp.status().is_success() {
            let kind = match resp.status() {
                reqwest::StatusCode::TOO_MANY_REQUESTS => LastFmErrorKind::RateLimited,
                _ => LastFmErrorKind::ApiError,
            };
            return Err(AppError::LastFm {
                kind,
                detail: format!("Last.fm HTTP {}", resp.status()),
            });
        }

        let body: response::SimilarArtistsResponse =
            resp.json().await.map_err(|e| AppError::LastFm {
                kind: LastFmErrorKind::InvalidResponse,
                detail: format!("Last.fm parse error: {e}"),
            })?;

        let similar = body
            .similarartists
            .artist
            .into_iter()
            .filter_map(|a| {
                let mbid = a.mbid.filter(|m| !m.is_empty())?;
                let score = a.matchfield.parse::<f32>().ok()?;
                Some(SimilarArtist {
                    mbid,
                    name: a.name,
                    similarity_score: score,
                })
            })
            .collect();

        Ok(similar)
    }
}
