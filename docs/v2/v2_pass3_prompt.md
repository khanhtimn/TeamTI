# TeamTI v2 — Pass 3 Implementation Prompt
## Enrichment Pipeline: AcoustID → MusicBrainz → Cover Art

---

### Context

Passes 1, 2.1, and 2.2 are complete. The following are stable:

- Domain entities, port traits, config, migrations (Pass 1)
- Full scan pipeline: Watcher → Classifier → Fingerprint Worker →
  Enrichment Orchestrator → `acoustid_tx` no-op consumer (Pass 2.1)
- Correctness and performance fixes from Pass 2.2

Pass 3 replaces the no-op AcoustID consumer with a real three-stage enrichment
pipeline: AcoustID fingerprint lookup → MusicBrainz metadata fetch → Cover Art
resolution. After this pass, newly indexed tracks move from `pending` to `done`
and become visible in Discord search and playback.

---

### Deliberate Deviation from Master Document

The master document specifies that `enrichment_status = 'done'` is set only
**after** lofty tag writeback (Pass 4). This pass deviates from that for
pragmatic reasons: without tag writeback, `done` is never reached and the bot
produces zero user-visible output.

**Resolution for this pass:**
After Cover Art completes, set `enrichment_status = 'done'`. Tags in the DB
are correct. File tags are not yet synchronized — that is Pass 4's job.

Pass 4 must re-open `done` tracks for tag writeback without changing their
`enrichment_status`. The distinction is: `done` means "enrichment complete in
DB and track is user-accessible." File tag synchronization is a separate,
post-enrichment operation.

This deviation is **scoped to this pass only** and must be clearly commented
wherever `enrichment_status = 'done'` is set without a preceding tag write.

---

### Acceptance Criteria

- [ ] `cargo build --workspace` — zero errors, zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] A new audio file dropped into `MEDIA_ROOT` eventually reaches
      `enrichment_status = 'done'` end-to-end (requires real AcoustID key)
- [ ] AcoustID and MusicBrainz each make at most 1 HTTP request per second
      (verified by log timestamps in integration test)
- [ ] A track with no AcoustID match is set to `unmatched`, NOT `failed`
- [ ] A track with AcoustID score < threshold is set to `low_confidence`
- [ ] An HTTP 5xx or timeout sets `failed` and increments `enrichment_attempts`
- [ ] After `failed_retry_limit` failures, `enrichment_status = 'exhausted'`
- [ ] Artist and Album rows are upserted without duplicates on repeated runs
- [ ] `cover.jpg` is written co-located with the track if art is found
- [ ] `albums.cover_art_path` is updated after a successful cover art fetch
- [ ] A track in `Unsorted/` (no album directory) is enriched correctly —
      cover art is skipped (no album dir), track reaches `done`
- [ ] `/search` and `/play` autocomplete return the newly enriched track

---

### Scope

| Crate | Action |
|---|---|
| `crates/adapters-acoustid/` | **Create** |
| `crates/adapters-musicbrainz/` | **Create** |
| `crates/adapters-cover-art/` | **Create** |
| `crates/application/` | **Extend** — add events, three worker modules |
| `crates/adapters-persistence/` | **Complete** — `update_enriched_metadata`, `TrackSearchPort` impl |
| `apps/bot/` | **Extend** — replace no-op consumer, wire full pipeline |

**Does NOT touch:** `adapters-discord`, `adapters-voice`, `adapters-watcher`,
`adapters-media-store`, tag writeback (Pass 4).

---

### Channel Topology (Full Pass 3 Pipeline)

```
[acoustid_rx from Pass 2 Orchestrator]
    │  AcoustIdRequest { track_id, fingerprint, duration_secs }   cap: 64
    ▼
AcoustID Worker  (application layer, GCRA 1 req/sec via adapters-acoustid)
    │  ToMusicBrainz { track_id, mbid, acoustid_id, confidence }  cap: 64
    ▼
MusicBrainz Worker  (application layer, GCRA 1 req/sec via adapters-musicbrainz)
    │  ToCoverArt { track_id, album_id, release_mbid, album_dir, blob_location }  cap: 64
    ▼
Cover Art Worker  (application layer, semaphore via adapters-cover-art)
    │  sets enrichment_status = 'done'
    ▼
[track visible in Discord search and playback]
```

All channels: `tokio::sync::mpsc`. Use exact capacities above.

---

### Step 1 — Extend `application/src/events.rs`

Add these message types. They are consumed by the three new worker modules.

```rust
use uuid::Uuid;

// (existing TrackScanned and AcoustIdRequest remain unchanged)

/// Emitted by AcoustID Worker on successful match.
/// Consumed by MusicBrainz Worker.
#[derive(Debug, Clone)]
pub struct ToMusicBrainz {
    pub track_id:      Uuid,
    pub mbid:          String,    // MusicBrainz Recording ID
    pub acoustid_id:   String,    // AcoustID track ID
    pub confidence:    f32,
    pub duration_secs: u32,       // carried through for MusicBrainz use
}

/// Emitted by MusicBrainz Worker after metadata is written to DB.
/// Consumed by Cover Art Worker.
#[derive(Debug, Clone)]
pub struct ToCoverArt {
    pub track_id:      Uuid,
    pub album_id:      Option<Uuid>,
    /// MusicBrainz Release ID — used for Cover Art Archive lookup.
    pub release_mbid:  String,
    /// Directory of the audio file, relative to MEDIA_ROOT.
    /// None if the file is in MEDIA_ROOT root (Unsorted/).
    pub album_dir:     Option<String>,
    /// Relative path to the audio file — used for embedded art fallback.
    pub blob_location: String,
}
```

---

### Step 2 — AcoustID Worker (`application/src/acoustid_worker.rs`)

This module owns the state machine transitions triggered by AcoustID results.
It calls `AcoustIdPort`, writes enrichment state to DB, and emits to the
MusicBrainz channel. It does NOT make HTTP calls directly — those go through
the port.

```rust
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::AppError;
use crate::events::{AcoustIdRequest, ToMusicBrainz};
use crate::ports::{AcoustIdPort, TrackRepository};

pub struct AcoustIdWorker {
    pub port:                      Arc<dyn AcoustIdPort>,
    pub repo:                      Arc<dyn TrackRepository>,
    pub confidence_threshold:      f32,
    pub failed_retry_limit:        u32,
    pub unmatched_retry_limit:     u32,
}

impl AcoustIdWorker {
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<AcoustIdRequest>,
        mb_tx: mpsc::Sender<ToMusicBrainz>,
    ) {
        while let Some(req) = rx.recv().await {
            // Rate limiting is enforced inside the port (governor GCRA).
            let result = self.port.lookup(&application::ports::enrichment::AudioFingerprint {
                fingerprint:   req.fingerprint.clone(),
                duration_secs: req.duration_secs,
            }).await;

            // Fetch current attempts count before deciding exhaustion
            let track = match self.repo.find_by_id(req.track_id).await {
                Ok(Some(t)) => t,
                Ok(None) => {
                    warn!("acoustid_worker: track {} no longer exists", req.track_id);
                    continue;
                }
                Err(e) => {
                    warn!("acoustid_worker: DB fetch failed: {e}");
                    continue;
                }
            };

            let attempts = track.enrichment_attempts + 1;

            match result {
                Ok(Some(m)) if m.score >= self.confidence_threshold => {
                    info!("acoustid: matched {} → MBID {} (score {:.2})",
                          req.track_id, m.recording_mbid, m.score);
                    // Update acoustid fields; status stays 'enriching'
                    let _ = self.repo.update_enrichment_status(
                        req.track_id,
                        &domain::EnrichmentStatus::Enriching,
                        track.enrichment_attempts,   // attempts unchanged on success
                        None,
                    ).await;
                    let _ = mb_tx.send(ToMusicBrainz {
                        track_id:      req.track_id,
                        mbid:          m.recording_mbid,
                        acoustid_id:   m.acoustid_id,
                        confidence:    m.score,
                        duration_secs: req.duration_secs,
                    }).await;
                }

                Ok(Some(m)) => {
                    // Score below threshold — low confidence
                    warn!("acoustid: low confidence for {} (score {:.2})",
                          req.track_id, m.score);
                    let status = if attempts >= self.failed_retry_limit {
                        domain::EnrichmentStatus::Exhausted
                    } else {
                        domain::EnrichmentStatus::LowConfidence
                    };
                    let _ = self.repo.update_enrichment_status(
                        req.track_id, &status, attempts,
                        Some(chrono::Utc::now()),
                    ).await;
                }

                Ok(None) => {
                    // No results
                    warn!("acoustid: no match for {}", req.track_id);
                    let status = if attempts >= self.unmatched_retry_limit {
                        domain::EnrichmentStatus::Exhausted
                    } else {
                        domain::EnrichmentStatus::Unmatched
                    };
                    let _ = self.repo.update_enrichment_status(
                        req.track_id, &status, attempts,
                        Some(chrono::Utc::now()),
                    ).await;
                }

                Err(e) => {
                    warn!("acoustid: error for {}: {e}", req.track_id);
                    let status = if attempts >= self.failed_retry_limit {
                        domain::EnrichmentStatus::Exhausted
                    } else {
                        domain::EnrichmentStatus::Failed
                    };
                    let _ = self.repo.update_enrichment_status(
                        req.track_id, &status, attempts,
                        Some(chrono::Utc::now()),
                    ).await;
                }
            }
        }
    }
}
```

---

### Step 3 — `crates/adapters-acoustid/`

#### `Cargo.toml`

```toml
[package]
name = "adapters-acoustid"
version = "0.1.0"
edition = "2021"

[dependencies]
application  = { path = "../application" }
domain       = { path = "../domain" }
reqwest      = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
governor     = { version = "0.6", default-features = false, features = ["std", "quanta"] }
nonzero_ext  = "0.3"
serde        = { workspace = true, features = ["derive"] }
serde_json   = "1"
tokio        = { workspace = true, features = ["sync"] }
tracing      = { workspace = true }
thiserror    = { workspace = true }
async-trait  = { workspace = true }
```

Add `serde_json = "1"` and `nonzero_ext = "0.3"` and
`governor = "0.6"` to workspace dependencies.

#### AcoustID response types (`src/response.rs`)

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AcoustIdResponse {
    pub status:  String,
    #[serde(default)]
    pub results: Vec<AcoustIdResult>,
}

#[derive(Debug, Deserialize)]
pub struct AcoustIdResult {
    pub id:          String,    // AcoustID track ID
    pub score:       f32,
    #[serde(default)]
    pub recordings:  Vec<AcoustIdRecording>,
}

#[derive(Debug, Deserialize)]
pub struct AcoustIdRecording {
    pub id: String,   // MusicBrainz Recording ID
}
```

#### `src/lib.rs`

```rust
mod response;

use std::sync::Arc;
use governor::{Quota, RateLimiter, state::{InMemoryState, NotKeyed}, clock::DefaultClock};
use nonzero_ext::nonzero;
use reqwest::Client;
use async_trait::async_trait;

use application::ports::enrichment::{AudioFingerprint, AcoustIdMatch};
use application::ports::AcoustIdPort;
use application::AppError;

use response::AcoustIdResponse;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct AcoustIdAdapter {
    client:  Client,
    limiter: Arc<Limiter>,
    api_key: String,
}

impl AcoustIdAdapter {
    pub fn new(api_key: String) -> Self {
        let limiter = Arc::new(RateLimiter::direct(
            Quota::per_second(nonzero!(1u32))
        ));
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build AcoustID HTTP client");
        Self { client, limiter, api_key }
    }
}

#[async_trait]
impl AcoustIdPort for AcoustIdAdapter {
    async fn lookup(
        &self,
        fp: &AudioFingerprint,
    ) -> Result<Option<AcoustIdMatch>, AppError> {
        // Block until the rate limiter permits the next request.
        // until_ready() yields to the tokio scheduler — no busy-wait.
        self.limiter.until_ready().await;

        let resp = self.client
            .post("https://api.acoustid.org/v2/lookup")
            .form(&[
                ("client",      self.api_key.as_str()),
                ("fingerprint", fp.fingerprint.as_str()),
                ("duration",    &fp.duration_secs.to_string()),
                ("meta",        "recordings+compress"),
            ])
            .send()
            .await
            .map_err(|e| AppError::ExternalApi(format!("AcoustID request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(AppError::ExternalApi(
                format!("AcoustID returned HTTP {}", resp.status())
            ));
        }

        let body: AcoustIdResponse = resp
            .json()
            .await
            .map_err(|e| AppError::ExternalApi(format!("AcoustID parse error: {e}")))?;

        if body.status != "ok" {
            return Err(AppError::ExternalApi(
                format!("AcoustID status: {}", body.status)
            ));
        }

        // Select the best result: highest score, must have at least one recording.
        let best = body.results
            .into_iter()
            .filter(|r| !r.recordings.is_empty())
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));

        Ok(best.map(|r| AcoustIdMatch {
            recording_mbid: r.recordings[0].id.clone(),
            score:          r.score,
            acoustid_id:    r.id,
        }))
    }
}
```

---

### Step 4 — MusicBrainz Worker (`application/src/musicbrainz_worker.rs`)

The MusicBrainz worker fetches recording metadata, upserts Artist and Album
rows, updates the track record, and fans out to Cover Art.

```rust
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use domain::{Artist, Album, TrackArtist, AlbumArtist, ArtistRole, EnrichmentStatus};
use crate::events::{ToMusicBrainz, ToCoverArt};
use crate::ports::{
    MusicBrainzPort, TrackRepository, ArtistRepository, AlbumRepository,
    enrichment::MbArtistCredit,
};

pub struct MusicBrainzWorker {
    pub port:         Arc<dyn MusicBrainzPort>,
    pub track_repo:   Arc<dyn TrackRepository>,
    pub artist_repo:  Arc<dyn ArtistRepository>,
    pub album_repo:   Arc<dyn AlbumRepository>,
    pub failed_retry_limit: u32,
}

impl MusicBrainzWorker {
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<ToMusicBrainz>,
        cover_tx: mpsc::Sender<ToCoverArt>,
    ) {
        while let Some(msg) = rx.recv().await {
            // Rate limiting enforced inside the port.
            let recording = match self.port.fetch_recording(&msg.mbid).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("musicbrainz: fetch failed for track {}: {e}", msg.track_id);
                    let track = self.track_repo.find_by_id(msg.track_id).await.ok().flatten();
                    let attempts = track.map(|t| t.enrichment_attempts + 1).unwrap_or(1);
                    let status = if attempts >= self.failed_retry_limit {
                        EnrichmentStatus::Exhausted
                    } else {
                        EnrichmentStatus::Failed
                    };
                    let _ = self.track_repo.update_enrichment_status(
                        msg.track_id, &status, attempts, Some(chrono::Utc::now()),
                    ).await;
                    continue;
                }
            };

            // --- Upsert Artists ---
            let mut primary_artist_display = String::new();
            for (i, credit) in recording.artist_credits.iter().enumerate() {
                let artist = Artist {
                    id:         Uuid::new_v4(),
                    name:       credit.name.clone(),
                    sort_name:  credit.sort_name.clone(),
                    mbid:       Some(credit.artist_mbid.clone()),
                    country:    None,
                    created_at: chrono::Utc::now(),
                };
                let upserted = match self.artist_repo.upsert(&artist).await {
                    Ok(a) => a,
                    Err(e) => {
                        warn!("musicbrainz: artist upsert failed: {e}");
                        continue;
                    }
                };

                // Build display string: "Artist A feat. Artist B"
                if i == 0 {
                    primary_artist_display = credit.name.clone();
                } else if let Some(ref phrase) = credit.join_phrase {
                    primary_artist_display.push_str(phrase);
                    primary_artist_display.push_str(&credit.name);
                }

                let role = if i == 0 {
                    ArtistRole::Primary
                } else {
                    ArtistRole::Featuring
                };

                let _ = self.artist_repo.upsert_track_artist(&TrackArtist {
                    track_id:  msg.track_id,
                    artist_id: upserted.id,
                    role,
                    position:  (i + 1) as i32,
                }).await;
            }

            // --- Upsert Album ---
            let album = Album {
                id:             Uuid::new_v4(),
                title:          recording.release_title.clone(),
                release_year:   recording.release_year,
                total_tracks:   None,
                total_discs:    Some(1),
                mbid:           Some(recording.release_mbid.clone()),
                cover_art_path: None,
                created_at:     chrono::Utc::now(),
            };
            let upserted_album = match self.album_repo.upsert(&album).await {
                Ok(a) => a,
                Err(e) => {
                    warn!("musicbrainz: album upsert failed: {e}");
                    continue;
                }
            };

            // Upsert AlbumArtist join rows
            for (i, credit) in recording.artist_credits.iter().enumerate() {
                if let Ok(Some(artist)) = self.artist_repo
                    .find_by_mbid(&credit.artist_mbid).await
                {
                    let _ = self.artist_repo.upsert_album_artist(&AlbumArtist {
                        album_id:  upserted_album.id,
                        artist_id: artist.id,
                        role:      if i == 0 { ArtistRole::Primary } else { ArtistRole::Featuring },
                        position:  (i + 1) as i32,
                    }).await;
                }
            }

            // --- Update track record ---
            let _ = self.track_repo.update_enriched_metadata(
                msg.track_id,
                &recording.title,
                &primary_artist_display,
                Some(upserted_album.id),
                recording.genre.as_deref(),
                recording.release_year,
                Some(&msg.mbid),
                Some(&msg.acoustid_id),
                Some(msg.confidence),
            ).await;

            info!("musicbrainz: enriched track {} → \"{}\"",
                  msg.track_id, recording.title);

            // --- Fan out to Cover Art ---
            // Derive album_dir from the track's blob_location.
            // album_dir is the parent directory of the file, relative to MEDIA_ROOT.
            let track = match self.track_repo.find_by_id(msg.track_id).await {
                Ok(Some(t)) => t,
                _ => continue,
            };

            let album_dir = std::path::Path::new(&track.blob_location)
                .parent()
                .filter(|p| *p != std::path::Path::new(""))
                .map(|p| p.to_string_lossy().into_owned());

            let _ = cover_tx.send(ToCoverArt {
                track_id:      msg.track_id,
                album_id:      Some(upserted_album.id),
                release_mbid:  recording.release_mbid,
                album_dir,
                blob_location: track.blob_location,
            }).await;
        }
    }
}
```

---

### Step 5 — `crates/adapters-musicbrainz/`

#### `Cargo.toml`

```toml
[package]
name = "adapters-musicbrainz"
version = "0.1.0"
edition = "2021"

[dependencies]
application  = { path = "../application" }
governor     = { version = "0.6", default-features = false, features = ["std", "quanta"] }
nonzero_ext  = "0.3"
reqwest      = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde        = { workspace = true, features = ["derive"] }
serde_json   = "1"
tokio        = { workspace = true }
tracing      = { workspace = true }
thiserror    = { workspace = true }
async-trait  = { workspace = true }
```

Do not use the `musicbrainz_rs` crate. Its response types do not map
cleanly to our domain model and its rate-limiting integration is opaque.
Use raw `reqwest` with explicit serde structs for full control.

#### MusicBrainz response types (`src/response.rs`)

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct MbRecordingResponse {
    pub id:               String,
    pub title:            String,
    #[serde(default)]
    #[serde(rename = "artist-credit")]
    pub artist_credit:    Vec<MbArtistCreditItem>,
    #[serde(default)]
    pub releases:         Vec<MbRelease>,
    #[serde(default)]
    pub genres:           Vec<MbGenre>,
}

#[derive(Debug, Deserialize)]
pub struct MbArtistCreditItem {
    pub name:        String,
    pub artist:      MbArtist,
    #[serde(default)]
    pub joinphrase:  String,
}

#[derive(Debug, Deserialize)]
pub struct MbArtist {
    pub id:          String,
    pub name:        String,
    #[serde(rename = "sort-name")]
    pub sort_name:   String,
}

#[derive(Debug, Deserialize)]
pub struct MbRelease {
    pub id:          String,
    pub title:       String,
    #[serde(default)]
    pub date:        String,        // "YYYY", "YYYY-MM", or "YYYY-MM-DD"
    #[serde(default)]
    #[serde(rename = "track-count")]
    pub track_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct MbGenre {
    pub name: String,
}
```

#### `src/lib.rs`

```rust
mod response;

use std::sync::Arc;
use governor::{Quota, RateLimiter, state::{InMemoryState, NotKeyed}, clock::DefaultClock};
use nonzero_ext::nonzero;
use reqwest::Client;
use async_trait::async_trait;

use application::ports::enrichment::{MbRecording, MbArtistCredit};
use application::ports::MusicBrainzPort;
use application::AppError;
use response::MbRecordingResponse;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct MusicBrainzAdapter {
    client:     Client,
    limiter:    Arc<Limiter>,
    user_agent: String,
}

impl MusicBrainzAdapter {
    pub fn new(user_agent: String) -> Self {
        let limiter = Arc::new(RateLimiter::direct(
            // Separate instance from AcoustID — independent 1 req/sec bucket.
            Quota::per_second(nonzero!(1u32))
        ));
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build MusicBrainz HTTP client");
        Self { client, limiter, user_agent }
    }
}

#[async_trait]
impl MusicBrainzPort for MusicBrainzAdapter {
    async fn fetch_recording(&self, mbid: &str) -> Result<MbRecording, AppError> {
        self.limiter.until_ready().await;

        let url = format!(
            "https://musicbrainz.org/ws/2/recording/{}?inc=releases+artists+genres",
            mbid
        );

        let resp = self.client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| AppError::ExternalApi(format!("MusicBrainz request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::NotFound(format!("MusicBrainz recording {mbid}")));
        }
        if !resp.status().is_success() {
            return Err(AppError::ExternalApi(
                format!("MusicBrainz HTTP {}", resp.status())
            ));
        }

        let body: MbRecordingResponse = resp
            .json()
            .await
            .map_err(|e| AppError::ExternalApi(format!("MusicBrainz parse error: {e}")))?;

        // Select the most relevant release (first available).
        let release = body.releases.into_iter().next();
        let release_mbid   = release.as_ref().map(|r| r.id.clone()).unwrap_or_default();
        let release_title  = release.as_ref().map(|r| r.title.clone()).unwrap_or_default();
        let release_year   = release.as_ref()
            .and_then(|r| r.date.split('-').next())
            .and_then(|y| y.parse::<i32>().ok());

        let artist_credits = body.artist_credit
            .into_iter()
            .map(|c| MbArtistCredit {
                artist_mbid:  c.artist.id,
                name:         c.name,
                sort_name:    c.artist.sort_name,
                join_phrase:  Some(c.joinphrase).filter(|s| !s.is_empty()),
            })
            .collect();

        let genre = body.genres.into_iter().next().map(|g| g.name);

        Ok(MbRecording {
            title:           body.title,
            artist_credits,
            release_mbid,
            release_title,
            release_year,
            genre,
        })
    }
}
```

---

### Step 6 — Cover Art Worker (`application/src/cover_art_worker.rs`)

```rust
use std::sync::Arc;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{info, warn};

use domain::EnrichmentStatus;
use crate::events::ToCoverArt;
use crate::ports::{CoverArtPort, AlbumRepository, TrackRepository};

pub struct CoverArtWorker {
    pub port:         Arc<dyn CoverArtPort>,
    pub track_repo:   Arc<dyn TrackRepository>,
    pub album_repo:   Arc<dyn AlbumRepository>,
    pub media_root:   PathBuf,
}

impl CoverArtWorker {
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<ToCoverArt>,
    ) {
        while let Some(msg) = rx.recv().await {
            self.process(msg).await;
        }
    }

    async fn process(&self, msg: ToCoverArt) {
        let cover_saved = self.try_resolve_cover(&msg).await;

        if let (Some(album_id), Some(rel_path)) = (msg.album_id, cover_saved) {
            let _ = self.album_repo
                .update_cover_art_path(album_id, &rel_path)
                .await;
        }

        // PASS 3 DEVIATION: set 'done' here without tag writeback.
        // Pass 4 will perform lofty tag writeback for 'done' tracks.
        // TODO(pass4): remove this and let Tag Writer set 'done'.
        let _ = self.track_repo.update_enrichment_status(
            msg.track_id,
            &EnrichmentStatus::Done,
            0,
            Some(chrono::Utc::now()),
        ).await;

        info!("cover_art: track {} → done", msg.track_id);
    }

    /// Returns Some(relative_path) if cover art was saved, None otherwise.
    async fn try_resolve_cover(&self, msg: &ToCoverArt) -> Option<String> {
        let album_dir = msg.album_dir.as_deref()?;
        let abs_album_dir = self.media_root.join(album_dir);
        let cover_path   = abs_album_dir.join("cover.jpg");
        let rel_cover    = format!("{album_dir}/cover.jpg");

        // Resolution order 1: cover.jpg already exists
        if cover_path.exists() {
            info!("cover_art: using existing cover.jpg for {album_dir}");
            return Some(rel_cover);
        }

        // Resolution order 2: Cover Art Archive
        match self.port.fetch_front(&msg.release_mbid).await {
            Ok(Some(bytes)) => {
                if let Err(e) = tokio::fs::create_dir_all(&abs_album_dir).await {
                    warn!("cover_art: mkdir failed for {album_dir}: {e}");
                    return None;
                }
                if let Err(e) = tokio::fs::write(&cover_path, &bytes).await {
                    warn!("cover_art: write cover.jpg failed: {e}");
                    return None;
                }
                info!("cover_art: fetched from CAA for {album_dir}");
                return Some(rel_cover);
            }
            Ok(None) => {}   // 404 — continue to next resolution
            Err(e) => warn!("cover_art: CAA fetch error: {e}"),
        }

        // Resolution order 3: embedded art in file tags
        let abs_file = self.media_root.join(&msg.blob_location);
        match self.port.extract_from_tags(&abs_file).await {
            Ok(Some(bytes)) => {
                if let Err(e) = tokio::fs::create_dir_all(&abs_album_dir).await {
                    warn!("cover_art: mkdir failed for {album_dir}: {e}");
                    return None;
                }
                if let Err(e) = tokio::fs::write(&cover_path, &bytes).await {
                    warn!("cover_art: write embedded art failed: {e}");
                    return None;
                }
                info!("cover_art: extracted embedded art for {album_dir}");
                return Some(rel_cover);
            }
            Ok(None) => {}
            Err(e) => warn!("cover_art: embedded art extraction error: {e}"),
        }

        // Resolution order 4: absent
        None
    }
}
```

---

### Step 7 — `crates/adapters-cover-art/`

#### `Cargo.toml`

```toml
[package]
name = "adapters-cover-art"
version = "0.1.0"
edition = "2021"

[dependencies]
application  = { path = "../application" }
lofty        = "0.21"
reqwest      = { version = "0.12", default-features = false, features = ["rustls-tls"] }
bytes        = { workspace = true }
tokio        = { workspace = true, features = ["sync"] }
tracing      = { workspace = true }
thiserror    = { workspace = true }
async-trait  = { workspace = true }
```

#### `src/lib.rs`

```rust
use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;
use reqwest::Client;
use tokio::sync::Semaphore;
use tracing::warn;

use application::ports::CoverArtPort;
use application::AppError;

pub struct CoverArtAdapter {
    client:    Client,
    semaphore: Arc<Semaphore>,
}

impl CoverArtAdapter {
    pub fn new(concurrency: usize) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            // Follow redirects — CAA returns 307 before the image URL.
            .redirect(reqwest::redirect::Policy::limited(5))
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
    async fn fetch_front(
        &self,
        release_mbid: &str,
    ) -> Result<Option<bytes::Bytes>, AppError> {
        let _permit = self.semaphore.acquire().await
            .map_err(|_| AppError::ExternalApi("cover art semaphore closed".into()))?;

        let url = format!(
            "https://coverartarchive.org/release/{release_mbid}/front-500"
        );

        let resp = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::ExternalApi(format!("CAA request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);  // No cover art available — not an error
        }
        if !resp.status().is_success() {
            return Err(AppError::ExternalApi(
                format!("CAA HTTP {}", resp.status())
            ));
        }

        let bytes = resp.bytes().await
            .map_err(|e| AppError::ExternalApi(format!("CAA body read error: {e}")))?;

        Ok(Some(bytes))
    }

    async fn extract_from_tags(
        &self,
        path: &Path,
    ) -> Result<Option<bytes::Bytes>, AppError> {
        // lofty is synchronous — run in spawn_blocking.
        // No SMB_READ_SEMAPHORE here: this is called from Cover Art Worker,
        // not the Fingerprint Worker. Cover art extraction is best-effort;
        // if it fails, we log and continue.
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let tagged = lofty::read_from_path(&path)?;
            let tag    = tagged.primary_tag().or_else(|| tagged.first_tag());

            let art = tag.and_then(|t| {
                t.pictures()
                 .iter()
                 .find(|p| {
                     p.pic_type() == lofty::picture::PictureType::CoverFront
                     || p.pic_type() == lofty::picture::PictureType::Other
                 })
                 .map(|p| bytes::Bytes::copy_from_slice(p.data()))
            });

            Ok::<_, lofty::error::LoftyError>(art)
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .map_err(|e| AppError::ExternalApi(format!("lofty tag read error: {e}")))
    }
}
```

---

### Step 8 — Complete Persistence Methods

#### `update_enriched_metadata` in `TrackRepositoryImpl`

```rust
async fn update_enriched_metadata(
    &self,
    id: Uuid,
    title: &str,
    artist_display: &str,
    album_id: Option<Uuid>,
    genre: Option<&str>,
    year: Option<i32>,
    mbid: Option<&str>,
    acoustid_id: Option<&str>,
    confidence: Option<f32>,
) -> Result<(), AppError> {
    sqlx::query!(
        r#"
        UPDATE tracks SET
            title                  = $2,
            artist_display         = $3,
            album_id               = $4,
            genre                  = $5,
            year                   = $6,
            mbid                   = $7,
            acoustid_id            = $8,
            enrichment_confidence  = $9,
            updated_at             = now()
        WHERE id = $1
        "#,
        id, title, artist_display, album_id,
        genre, year, mbid, acoustid_id, confidence
    )
    .execute(&self.pool)
    .await?;
    Ok(())
}
```

#### `ArtistRepositoryImpl::upsert`

```rust
async fn upsert(&self, artist: &Artist) -> Result<Artist, AppError> {
    let row = sqlx::query_as!(
        Artist,
        r#"
        INSERT INTO artists (id, name, sort_name, mbid, country, created_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (mbid) DO UPDATE SET
            name      = EXCLUDED.name,
            sort_name = EXCLUDED.sort_name,
            country   = COALESCE(EXCLUDED.country, artists.country)
        RETURNING *
        "#,
        artist.id, artist.name, artist.sort_name,
        artist.mbid, artist.country, artist.created_at
    )
    .fetch_one(&self.pool)
    .await?;
    Ok(row)
}
```

#### `AlbumRepositoryImpl::upsert`

```rust
async fn upsert(&self, album: &Album) -> Result<Album, AppError> {
    let row = sqlx::query_as!(
        Album,
        r#"
        INSERT INTO albums (id, title, release_year, total_tracks, total_discs,
                            mbid, cover_art_path, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (mbid) DO UPDATE SET
            title        = EXCLUDED.title,
            release_year = COALESCE(EXCLUDED.release_year, albums.release_year),
            total_tracks = COALESCE(EXCLUDED.total_tracks, albums.total_tracks)
        RETURNING *
        "#,
        album.id, album.title, album.release_year, album.total_tracks,
        album.total_discs, album.mbid, album.cover_art_path, album.created_at
    )
    .fetch_one(&self.pool)
    .await?;
    Ok(row)
}
```

#### `TrackSearchPort` implementation

Search only returns `done` tracks. Use hybrid FTS + trigram:

```rust
async fn search(
    &self,
    query: &str,
    limit: usize,
) -> Result<Vec<TrackSummary>, AppError> {
    let normalized = query.to_lowercase();
    let rows = sqlx::query_as!(
        TrackSummary,
        r#"
        SELECT id, title, artist_display, album_id, duration_ms, blob_location
        FROM tracks
        WHERE enrichment_status = 'done'
          AND (
            search_vector @@ plainto_tsquery('music_simple', $1)
            OR search_text ILIKE '%' || $2 || '%'
          )
        ORDER BY
            ts_rank(search_vector, plainto_tsquery('music_simple', $1)) DESC,
            similarity(search_text, $2) DESC
        LIMIT $3
        "#,
        normalized,
        normalized,
        limit as i64
    )
    .fetch_all(&self.pool)
    .await?;
    Ok(rows)
}

async fn autocomplete(
    &self,
    prefix: &str,
    limit: usize,
) -> Result<Vec<TrackSummary>, AppError> {
    let pattern = format!("{}%", prefix.to_lowercase());
    let rows = sqlx::query_as!(
        TrackSummary,
        r#"
        SELECT id, title, artist_display, album_id, duration_ms, blob_location
        FROM tracks
        WHERE enrichment_status = 'done'
          AND search_text LIKE $1
        ORDER BY title ASC
        LIMIT $2
        "#,
        pattern,
        limit as i64
    )
    .fetch_all(&self.pool)
    .await?;
    Ok(rows)
}
```

---

### Step 9 — Wire Pipeline in `apps/bot/main.rs`

Replace the no-op AcoustID consumer from Pass 2.1 with the full pipeline.

```rust
use adapters_acoustid::AcoustIdAdapter;
use adapters_musicbrainz::MusicBrainzAdapter;
use adapters_cover_art::CoverArtAdapter;
use application::{
    AcoustIdWorker, MusicBrainzWorker, CoverArtWorker,
    events::{ToMusicBrainz, ToCoverArt},
};

// --- Build adapters ---
let acoustid_adapter = Arc::new(
    AcoustIdAdapter::new(config.acoustid_api_key.clone())
);
let mb_adapter = Arc::new(
    MusicBrainzAdapter::new(config.mb_user_agent.clone())
);
let cover_art_adapter = Arc::new(
    CoverArtAdapter::new(config.cover_art_concurrency)
);

// --- Build channels ---
// acoustid_tx already exists from Pass 2 (connected to Orchestrator)
let (mb_tx,    mb_rx)    = mpsc::channel::<ToMusicBrainz>(64);
let (cover_tx, cover_rx) = mpsc::channel::<ToCoverArt>(64);

// --- Spawn workers ---

// AcoustID Worker
{
    let worker = Arc::new(AcoustIdWorker {
        port:                  acoustid_adapter,
        repo:                  Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        confidence_threshold:  config.enrichment_confidence_threshold,
        failed_retry_limit:    config.failed_retry_limit,
        unmatched_retry_limit: config.unmatched_retry_limit,
    });
    let tok = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = worker.run(acoustid_rx, mb_tx) => {}
        }
    });
}

// MusicBrainz Worker
{
    let worker = Arc::new(MusicBrainzWorker {
        port:               mb_adapter,
        track_repo:         Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        artist_repo:        Arc::clone(&artist_repo) as Arc<dyn ArtistRepository>,
        album_repo:         Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
        failed_retry_limit: config.failed_retry_limit,
    });
    let tok = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = worker.run(mb_rx, cover_tx) => {}
        }
    });
}

// Cover Art Worker
{
    let worker = Arc::new(CoverArtWorker {
        port:       cover_art_adapter,
        track_repo: Arc::clone(&track_repo) as Arc<dyn TrackRepository>,
        album_repo: Arc::clone(&album_repo) as Arc<dyn AlbumRepository>,
        media_root: config.media_root.clone(),
    });
    let tok = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = tok.cancelled() => {}
            _ = worker.run(cover_rx) => {}
        }
    });
}
```

---

### Step 10 — Integration Smoke Test

Add `tests/enrichment_smoke.rs` in `apps/bot/` or a dedicated test crate.
Guard with `TEST_DATABASE_URL` and `TEST_ACOUSTID_KEY` env vars.

```
test: single file placed in MEDIA_ROOT reaches enrichment_status = 'done'
  timing: assert done within 120 seconds of file creation
  assert: tracks.title is not the filename stem (enriched from MusicBrainz)
  assert: tracks.artist_display is not null
  assert: tracks.album_id is not null
  assert: artists table has at least one row for this track
  assert: albums table has a row for the release
  assert: /search returns the track (TrackSearchPort::search)

test: Unsorted file (no album dir) reaches done without cover.jpg write
  assert: enrichment_status = 'done'
  assert: albums.cover_art_path is NULL for this album
  no filesystem write to MEDIA_ROOT/cover.jpg at root level

test: rate limiter — 3 concurrent tracks
  assert: total wall-clock time >= 2s (3 AcoustID calls at 1 req/sec)
  assert: total wall-clock time >= 2s for MusicBrainz (independent limiter)
```

---

### Invariants (Pass 3 Specific)

All 15 master document invariants apply. Additional for this pass:

| Rule | Detail |
|---|---|
| AcoustID and MusicBrainz rate limiters are separate governor instances | Never share a single Limiter between them |
| `until_ready().await` is always called before every HTTP request | Never call the HTTP client without first awaiting the limiter |
| No `enrichment_status = 'done'` set without the Pass 3 deviation comment | Every call site that sets `done` must have `// TODO(pass4)` comment |
| Cover art is never stored in Postgres | Only `cover_art_path TEXT` in `albums`; image bytes written to disk only |
| `MB_USER_AGENT` header is required on every MusicBrainz request | Omitting it causes IP ban; verify in integration test by checking request headers |
| No `reqwest::blocking` anywhere | All HTTP is async; blocking client banned by Invariant 15 spirit |

---

### REFERENCE

docs/v2/v2_master.md