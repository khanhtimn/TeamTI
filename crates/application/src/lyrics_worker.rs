use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, error, info, instrument};

use crate::events::{ToCoverArt, ToLyrics};
use crate::ports::enrichment::LyricsProviderPort;
use crate::ports::repository::TrackRepository;

pub struct LyricsWorker {
    pub port: Arc<dyn LyricsProviderPort>,
    pub track_repo: Arc<dyn TrackRepository>,
}

impl LyricsWorker {
    pub async fn run(self: Arc<Self>, mut rx: Receiver<ToLyrics>, tx: Sender<ToCoverArt>) {
        while let Some(event) = rx.recv().await {
            let worker = Arc::clone(&self);
            let tx = tx.clone();

            // Unbounded concurrency since this is mostly I/O (LRCLIB + local disk checks).
            tokio::spawn(async move {
                if let Err(e) = worker.process(event.clone(), tx).await {
                    error!(
                        error = %e,
                        track_id = %event.track_id,
                        correlation_id = %event.correlation_id,
                        "Lyrics lookup failed"
                    );
                }
            });
        }
    }

    #[instrument(skip(self, tx), fields(track_id = %event.track_id, correlation_id = %event.correlation_id))]
    async fn process(
        &self,
        event: ToLyrics,
        tx: Sender<ToCoverArt>,
    ) -> Result<(), crate::AppError> {
        debug!("Processing lyrics enrichment");

        let track_opt = self.track_repo.find_by_id(event.track_id).await?;
        let track = match track_opt {
            Some(t) => t,
            None => {
                error!("Track not found in DB during Lyrics Enrichment");
                return Ok(());
            }
        };

        // If lyrics already exist (embedded during scan), skip LRCLIB.
        if track.lyrics.is_some() {
            debug!("Lyrics already populated from embedded tag, skipping lookup");
        } else {
            // Query port (local sidecar first, then LRCLIB)
            if let Some(fetched_lyrics) = self
                .port
                .fetch_lyrics(
                    &event.blob_location,
                    &event.track_name,
                    &event.artist_name,
                    event.album_name.as_deref(),
                    event.duration_secs,
                )
                .await?
            {
                info!(
                    track_name = %event.track_name,
                    artist_name = %event.artist_name,
                    "Found lyrics"
                );
                self.track_repo
                    .update_lyrics(event.track_id, &fetched_lyrics)
                    .await?;
            } else {
                debug!("No lyrics found");
            }
        }

        // Emit to Cover Art Worker
        let out_evt = ToCoverArt {
            track_id: event.track_id,
            album_id: track.album_id,
            release_mbid: event.release_mbid,
            album_dir: event.album_dir,
            blob_location: event.blob_location,
            enrichment_attempts: event.enrichment_attempts,
            correlation_id: event.correlation_id,
        };

        if let Err(e) = tx.send(out_evt).await {
            error!(error = %e, "Failed to send ToCoverArt event");
        }

        Ok(())
    }
}
