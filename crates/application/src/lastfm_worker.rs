//! Background worker: caches Last.fm similar-artist data during enrichment.
//!
//! Sits between MusicBrainz Worker (receives `ToLastFm`) and Lyrics Worker
//! (emits `ToLyrics`). Always forwards `ToLyrics` even if Last.fm fails.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AppError;
use crate::events::{ToLastFm, ToLyrics};
use crate::ports::LastFmPort;
use crate::ports::repository::TrackRepository;

pub struct LastFmWorker {
    pub port: Arc<dyn LastFmPort>,
    pub repo: Arc<dyn TrackRepository>,
}

impl LastFmWorker {
    pub async fn run(&self, mut rx: mpsc::Receiver<ToLastFm>, lyrics_tx: mpsc::Sender<ToLyrics>) {
        info!(operation = "lastfm_worker.start", "Last.fm worker started");

        // Fast-path bypass if port is essentially dead (no API key configured)
        // If the API key is not configured, we should just NOT fetch.
        // We can check local config via std::env::var("LASTFM_API_KEY").
        let has_api_key = std::env::var("LASTFM_API_KEY")
            .ok()
            .as_ref()
            .is_some_and(|s| !s.is_empty());
        if !has_api_key {
            warn!(
                operation = "lastfm_worker.start",
                "LASTFM_API_KEY not set — Last.fm similarity disabled"
            );
        }

        while let Some(msg) = rx.recv().await {
            let correlation_id = msg.correlation_id;

            // Only attempt fetch if API is configured
            if has_api_key {
                // Batched cache check
                let mut cached_mbids = std::collections::HashSet::new();
                if let Ok(cached) = self
                    .repo
                    .get_cached_similar_artists(&msg.artist_mbids)
                    .await
                {
                    for c in cached {
                        cached_mbids.insert(c);
                    }
                }

                // Look up similar artists for each artist MBID
                for mbid in &msg.artist_mbids {
                    if cached_mbids.contains(mbid) {
                        continue; // Skip already cached
                    }

                    if let Err(e) = self.cache_similar_artists(mbid, correlation_id).await {
                        warn!(
                            artist_mbid = mbid,
                            error = %e,
                            correlation_id = %correlation_id,
                            operation = "lastfm_worker.fetch_failed",
                            "Last.fm similar artist fetch failed (non-fatal)"
                        );
                        // Non-fatal — continue to next artist
                    }
                }
            }

            // Always forward to Lyrics Worker, regardless of Last.fm success
            let lyrics = ToLyrics {
                track_id: msg.track_id,
                release_mbid: msg.release_mbid,
                album_dir: msg.album_dir,
                blob_location: msg.blob_location,
                enrichment_attempts: msg.enrichment_attempts,
                correlation_id: msg.correlation_id,
                track_name: msg.track_name,
                artist_name: msg.artist_name,
                album_name: msg.album_name,
                duration_secs: msg.duration_secs,
            };

            if lyrics_tx.send(lyrics).await.is_err() {
                warn!(
                    operation = "lastfm_worker.lyrics_tx_closed",
                    "Lyrics channel closed — stopping Last.fm worker"
                );
                return;
            }
        }
    }

    async fn cache_similar_artists(
        &self,
        artist_mbid: &str,
        correlation_id: Uuid,
    ) -> Result<(), AppError> {
        let similar = self.port.get_similar_artists(artist_mbid).await?;

        if similar.is_empty() {
            tracing::debug!(
                artist_mbid,
                correlation_id = %correlation_id,
                operation = "lastfm_worker.no_similar",
                "No similar artists from Last.fm"
            );
            return Ok(());
        }

        // Store via bulk upsert into similar_artists table
        // This is done via the TrackRepository (which has DB access)
        // but the design calls for it in the recommendation repo.
        // For now, we store via a dedicated method on TrackRepository.
        self.repo
            .upsert_similar_artists(artist_mbid, &similar)
            .await?;

        tracing::debug!(
            artist_mbid,
            count = similar.len(),
            correlation_id = %correlation_id,
            operation = "lastfm_worker.cached",
            "Cached similar artists"
        );

        Ok(())
    }
}
