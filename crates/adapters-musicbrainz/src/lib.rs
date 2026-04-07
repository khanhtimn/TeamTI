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
use application::error::MusicBrainzErrorKind;
use application::ports::MusicBrainzPort;
use application::ports::enrichment::{MbArtistCredit, MbRecording};
use response::{MbRecordingResponse, release_priority};

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct MusicBrainzAdapter {
    client: Client,
    limiter: Arc<Limiter>,
    user_agent: String,
}

impl MusicBrainzAdapter {
    #[must_use]
    pub fn new(user_agent: String) -> Self {
        // C2 fix: validate User-Agent format at startup. MusicBrainz requires
        // "AppName/version (contact-url)" format. Missing or malformed UA
        // causes silent IP bans hours after deployment.
        assert!(
            user_agent.contains('/') && user_agent.contains('('),
            "MB_USER_AGENT must be in format 'AppName/version (contact-url)'. \
             Got: {user_agent:?}"
        );
        let limiter = Arc::new(RateLimiter::direct(
            // Separate instance from AcoustID — independent 1 req/sec bucket.
            Quota::per_second(nonzero!(1u32)),
        ));
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build MusicBrainz HTTP client");
        Self {
            client,
            limiter,
            user_agent,
        }
    }
}

#[async_trait]
impl MusicBrainzPort for MusicBrainzAdapter {
    async fn fetch_recording(&self, mbid: &str) -> Result<MbRecording, AppError> {
        self.limiter.until_ready().await;

        let url = format!(
            "https://musicbrainz.org/ws/2/recording/{mbid}?inc=releases+artists+genres+release-groups+isrcs+work-rels"
        );

        let resp = self
            .client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::HttpError,
                detail: format!("MusicBrainz request failed: {e}"),
            })?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::NotFound,
                detail: format!("MusicBrainz recording {mbid}"),
            });
        }
        if !resp.status().is_success() {
            let kind = match resp.status() {
                reqwest::StatusCode::TOO_MANY_REQUESTS => MusicBrainzErrorKind::RateLimited,
                s if s.is_server_error() => MusicBrainzErrorKind::ServiceUnavailable,
                _ => MusicBrainzErrorKind::HttpError,
            };
            return Err(AppError::MusicBrainz {
                kind,
                detail: format!("MusicBrainz HTTP {}", resp.status()),
            });
        }

        let body_text = resp.text().await.map_err(|e| AppError::MusicBrainz {
            kind: MusicBrainzErrorKind::InvalidResponse,
            detail: format!("failed to read MusicBrainz response body: {e}"),
        })?;
        let body: MbRecordingResponse =
            serde_json::from_str(&body_text).map_err(|e| AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::InvalidResponse,
                detail: format!("MusicBrainz JSON parse error: {e} — body: {body_text}",),
            })?;

        // B4 fix: select the most relevant release by priority.
        // Prefer official studio albums over compilations/bootlegs.
        let release = body.releases.into_iter().min_by_key(release_priority);
        let release_mbid = release.as_ref().map(|r| r.id.clone()).unwrap_or_default();
        let release_title = release
            .as_ref()
            .map(|r| r.title.clone())
            .unwrap_or_default();

        let mut release_date = None;
        let mut release_year = None;
        if let Some(r) = release.as_ref()
            && !r.date.is_empty()
        {
            // Try parsing full date "YYYY-MM-DD"
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&r.date, "%Y-%m-%d") {
                release_date = Some(d);
                release_year = Some(
                    d.format("%Y")
                        .to_string()
                        .parse::<i32>()
                        .unwrap_or_default(),
                );
            } else if let Ok(y) = r.date.split('-').next().unwrap_or_default().parse::<i32>() {
                release_year = Some(y);
            }
        }

        let barcode = release.as_ref().and_then(|r| r.barcode.clone());
        let record_label = release.as_ref().and_then(|r| {
            r.label_info
                .as_ref()?
                .first()
                .and_then(|li| li.label.as_ref().map(|l| l.name.clone()))
        });

        let artist_credits: Vec<MbArtistCredit> = body
            .artist_credit
            .iter()
            .map(|c| MbArtistCredit {
                artist_mbid: c.artist.id.clone(),
                name: c.name.clone(),
                sort_name: c.artist.sort_name.clone(),
                join_phrase: Some(c.joinphrase.clone()).filter(|s| !s.is_empty()),
            })
            .collect();

        let mut genres: Vec<String> = body.genres.into_iter().map(|g| g.name).collect();

        // Fallback: if recording lacks genres, check the chosen release group
        if genres.is_empty()
            && let Some(rg) = release.as_ref().and_then(|r| r.release_group.as_ref())
        {
            genres = rg.genres.iter().map(|g| g.name.clone()).collect();
        }

        // Fallback: check primary artist
        if genres.is_empty()
            && let Some(ac) = body.artist_credit.first()
        {
            genres = ac.artist.genres.iter().map(|g| g.name.clone()).collect();
        }

        // Extract first ISRC if available
        let isrc = body.isrcs.first().cloned();

        // Extract linked Work MBID from forward "performance" relations
        let work_mbid = body
            .relations
            .into_iter()
            .find(|r| {
                r.rel_type == "performance"
                    && r.direction.as_deref() == Some("forward")
                    && r.target.as_deref() == Some("work")
            })
            .and_then(|r| r.work.map(|w| w.id));

        Ok(MbRecording {
            title: body.title,
            artist_credits,
            release_mbid,
            release_title,
            release_year,
            release_date,
            genres,
            barcode,
            record_label,
            isrc,
            work_mbid,
        })
    }

    async fn fetch_work_credits(
        &self,
        work_mbid: &str,
    ) -> Result<application::ports::enrichment::MbWorkCredits, AppError> {
        self.limiter.until_ready().await;

        let url = format!("https://musicbrainz.org/ws/2/work/{work_mbid}?inc=artist-rels");

        let resp = self
            .client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::HttpError,
                detail: format!("MusicBrainz request failed: {e}"),
            })?;

        if !resp.status().is_success() {
            let kind = match resp.status() {
                reqwest::StatusCode::TOO_MANY_REQUESTS => MusicBrainzErrorKind::RateLimited,
                reqwest::StatusCode::NOT_FOUND => MusicBrainzErrorKind::NotFound,
                s if s.is_server_error() => MusicBrainzErrorKind::ServiceUnavailable,
                _ => MusicBrainzErrorKind::HttpError,
            };
            return Err(AppError::MusicBrainz {
                kind,
                detail: format!("MusicBrainz HTTP {}", resp.status()),
            });
        }

        let body_text = resp.text().await.map_err(|e| AppError::MusicBrainz {
            kind: MusicBrainzErrorKind::InvalidResponse,
            detail: format!("failed to read MusicBrainz response body: {e}"),
        })?;
        let body: response::MbWorkResponse =
            serde_json::from_str(&body_text).map_err(|e| AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::InvalidResponse,
                detail: format!(
                    "MusicBrainz JSON parse error: {e} — raw body (first 500 chars): {}",
                    &body_text[..body_text.len().min(500)]
                ),
            })?;

        let mut composers = Vec::new();
        let mut lyricists = Vec::new();

        for rel in body.relations {
            if rel.target.as_deref() == Some("artist")
                && let Some(artist) = rel.artist
            {
                let credit = application::ports::enrichment::MbArtistCredit {
                    artist_mbid: artist.id,
                    name: artist.name,
                    sort_name: artist.sort_name.unwrap_or_default(),
                    join_phrase: None,
                };
                if rel.rel_type == "composer" {
                    composers.push(credit);
                } else if rel.rel_type == "lyricist" {
                    lyricists.push(credit);
                }
            }
        }

        Ok(application::ports::enrichment::MbWorkCredits {
            composers,
            lyricists,
        })
    }

    async fn fetch_release_label(&self, release_mbid: &str) -> Result<Option<String>, AppError> {
        self.limiter.until_ready().await;

        let url = format!("https://musicbrainz.org/ws/2/release/{release_mbid}?inc=labels");

        let resp = self
            .client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::HttpError,
                detail: format!("MusicBrainz request failed: {e}"),
            })?;

        if !resp.status().is_success() {
            let kind = match resp.status() {
                reqwest::StatusCode::TOO_MANY_REQUESTS => MusicBrainzErrorKind::RateLimited,
                reqwest::StatusCode::NOT_FOUND => MusicBrainzErrorKind::NotFound,
                s if s.is_server_error() => MusicBrainzErrorKind::ServiceUnavailable,
                _ => MusicBrainzErrorKind::HttpError,
            };
            return Err(AppError::MusicBrainz {
                kind,
                detail: format!("MusicBrainz HTTP {}", resp.status()),
            });
        }

        let body_text = resp.text().await.map_err(|e| AppError::MusicBrainz {
            kind: MusicBrainzErrorKind::InvalidResponse,
            detail: format!("failed to read MusicBrainz response body: {e}"),
        })?;
        let body: response::MbReleaseResponse =
            serde_json::from_str(&body_text).map_err(|e| AppError::MusicBrainz {
                kind: MusicBrainzErrorKind::InvalidResponse,
                detail: format!(
                    "MusicBrainz JSON parse error: {e} — raw body (first 500 chars): {}",
                    &body_text[..body_text.len().min(500)]
                ),
            })?;

        let label_name = body
            .label_info
            .into_iter()
            .find_map(|li| li.label.map(|l| l.name));

        Ok(label_name)
    }
}
