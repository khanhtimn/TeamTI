use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::events::{ToCoverArt, ToTagWriter};
use crate::ports::{AlbumRepository, CoverArtPort, TrackRepository};
use domain::EnrichmentStatus;

pub struct CoverArtWorker {
    pub port: Arc<dyn CoverArtPort>,
    pub track_repo: Arc<dyn TrackRepository>,
    pub album_repo: Arc<dyn AlbumRepository>,
    pub media_root: PathBuf,
    /// B1 fix: non-optional. In production, always wired to the Tag Writer
    /// channel. In tests, use a dropped receiver: `let (tx, _) = mpsc::channel(1)`.
    pub tag_writer_tx: mpsc::Sender<ToTagWriter>,
}

impl CoverArtWorker {
    // B6 fix: spawn a task per message — let the adapter's semaphore
    // control actual HTTP concurrency, instead of processing sequentially.
    pub async fn run(self: Arc<Self>, mut rx: mpsc::Receiver<ToCoverArt>) {
        while let Some(msg) = rx.recv().await {
            let worker = Arc::clone(&self);
            tokio::spawn(async move {
                worker.process(msg).await;
            });
        }
    }

    async fn process(&self, msg: ToCoverArt) {
        let cover_saved = self.try_resolve_cover(&msg).await;

        if let (Some(album_id), Some(rel_path)) = (msg.album_id, cover_saved) {
            let _ = self
                .album_repo
                .update_cover_art_path(album_id, &rel_path)
                .await;
        }

        // PASS 3 DEVIATION (now permanent): set 'done' here.
        // Tag writeback is a separate eventual-consistency step.
        // DESIGN-4 fix: preserve actual enrichment_attempts instead of resetting to 0.
        let _ = self
            .track_repo
            .update_enrichment_status(
                msg.track_id,
                &EnrichmentStatus::Done,
                msg.enrichment_attempts,
                Some(chrono::Utc::now()),
            )
            .await;

        // Fan-out to Tag Writer for file tag synchronization.
        let _ = self
            .tag_writer_tx
            .send(ToTagWriter {
                track_id: msg.track_id,
                blob_location: msg.blob_location.clone(),
                correlation_id: msg.correlation_id,
            })
            .await;

        info!(
            "cover_art: track {} → done, queued for tag writeback",
            msg.track_id
        );
    }

    /// Returns Some(relative_path) if cover art was saved, None otherwise.
    async fn try_resolve_cover(&self, msg: &ToCoverArt) -> Option<String> {
        let album_dir = msg.album_dir.as_deref()?;
        let abs_album_dir = self.media_root.join(album_dir);
        let cover_path = abs_album_dir.join("cover.jpg");
        let rel_cover = format!("{album_dir}/cover.jpg");

        // Resolution order 1: cover.jpg already exists
        if cover_path.exists() {
            info!(
                album_id = ?msg.album_id,
                correlation_id = %msg.correlation_id,
                path = %cover_path.display(),
                "cover_art: using existing cover.jpg"
            );
            return Some(rel_cover);
        }

        // Resolution order 2: Cover Art Archive
        match self.port.fetch_front(&msg.release_mbid).await {
            Ok(Some(bytes)) => {
                if let Err(e) = tokio::fs::create_dir_all(&abs_album_dir).await {
                    warn!(
                        album_id = ?msg.album_id,
                        correlation_id = %msg.correlation_id,
                        error = %e,
                        "cover_art: mkdir failed"
                    );
                    return None;
                }
                if let Err(e) = tokio::fs::write(&cover_path, &bytes).await {
                    warn!(
                        album_id = ?msg.album_id,
                        correlation_id = %msg.correlation_id,
                        error = %e,
                        "cover_art: write cover.jpg failed"
                    );
                    return None;
                }
                info!(
                    album_id = ?msg.album_id,
                    correlation_id = %msg.correlation_id,
                    "cover_art: fetched from CAA"
                );
                return Some(rel_cover);
            }
            Ok(None) => {} // 404 — continue to next resolution
            Err(e) => warn!(
                album_id = ?msg.album_id,
                correlation_id = %msg.correlation_id,
                error = %e,
                "cover_art: CAA fetch error"
            ),
        }

        // Resolution order 3: embedded art in file tags
        let abs_file = self.media_root.join(&msg.blob_location);
        match self.port.extract_from_tags(&abs_file).await {
            Ok(Some(bytes)) => {
                if let Err(e) = tokio::fs::create_dir_all(&abs_album_dir).await {
                    warn!(
                        album_id = ?msg.album_id,
                        correlation_id = %msg.correlation_id,
                        error = %e,
                        "cover_art: mkdir failed"
                    );
                    return None;
                }
                if let Err(e) = tokio::fs::write(&cover_path, &bytes).await {
                    warn!(
                        album_id = ?msg.album_id,
                        correlation_id = %msg.correlation_id,
                        error = %e,
                        "cover_art: write embedded art failed"
                    );
                    return None;
                }
                info!(
                    album_id = ?msg.album_id,
                    correlation_id = %msg.correlation_id,
                    "cover_art: extracted embedded art"
                );
                return Some(rel_cover);
            }
            Ok(None) => {}
            Err(e) => warn!(
                album_id = ?msg.album_id,
                correlation_id = %msg.correlation_id,
                error = %e,
                "cover_art: embedded art extraction error"
            ),
        }

        // Resolution order 4: absent
        None
    }
}
