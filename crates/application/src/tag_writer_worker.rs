use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info, warn};

use crate::AppError;
use crate::events::ToTagWriter;
use crate::ports::file_ops::{FileTagWriterPort, TagData};
use crate::ports::repository::{AlbumRepository, ArtistRepository, TrackRepository};
use crate::ports::search::MusicSearchPort;

/// A1 fix: `TagWriterWorker` has NO `smb_semaphore` field.
/// The `FileTagWriterPort` owns SMB semaphore acquisition internally.
/// The worker only owns a `task_semaphore` for concurrency limiting (C1 fix).
pub struct TagWriterWorker {
    pub tag_writer: Arc<dyn FileTagWriterPort>,
    pub track_repo: Arc<dyn TrackRepository>,
    pub album_repo: Arc<dyn AlbumRepository>,
    pub artist_repo: Arc<dyn ArtistRepository>,
    pub search_port: Arc<dyn MusicSearchPort>,
    /// C1 fix: limits concurrent tag write tasks to avoid unbounded spawning.
    /// Default: 2 (D2 fix: each task loads the full file into memory).
    pub task_semaphore: Arc<Semaphore>,
}

impl TagWriterWorker {
    /// C1 fix: acquire task_semaphore before spawning to limit concurrency.
    pub async fn run(self: Arc<Self>, mut rx: mpsc::Receiver<ToTagWriter>) {
        while let Some(msg) = rx.recv().await {
            let permit = self
                .task_semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("task semaphore closed");
            let worker = Arc::clone(&self);
            tokio::spawn(async move {
                let _permit = permit; // drops when task completes
                let track_id = msg.track_id;
                let correlation_id = msg.correlation_id;
                if let Err(e) = worker.process(msg).await {
                    warn!(
                        track_id = %track_id,
                        correlation_id = %correlation_id,
                        error = %e,
                        "tag_writer: error"
                    );
                }
            });
        }
    }

    /// A1 fix: process() delegates entirely to the port for file operations.
    /// No SMB semaphore acquisition here — the port handles it.
    async fn process(&self, msg: ToTagWriter) -> Result<(), AppError> {
        // Fetch full track + album data BEFORE any file operations.
        // DB reads are async and fast.
        let track = self
            .track_repo
            .find_by_id(msg.track_id)
            .await?
            .ok_or_else(|| AppError::TrackNotFound { id: msg.track_id })?;

        let album = match track.album_id {
            Some(album_id) => self.album_repo.find_by_id(album_id).await?,
            None => None,
        };

        let credits = self.track_repo.get_credits(msg.track_id).await?;

        let tags = TagData {
            title: track.title.clone(),
            artist: track.artist_display.clone().unwrap_or_default(),
            album_title: album.as_ref().map(|a| a.title.clone()),
            year: track.year,
            genres: track.genres.unwrap_or_default(),
            track_number: track.track_number,
            disc_number: track.disc_number,
            bpm: track.bpm,
            isrc: track.isrc.clone(),
            composers: credits.composers,
            lyricists: credits.lyricists,
            lyrics: track.lyrics.clone(),
        };

        // Port handles SMB semaphore acquisition and spawn_blocking internally.
        let result = self
            .tag_writer
            .write_tags(&msg.blob_location, &tags)
            .await?;

        // A4 fix: update_file_tags_written includes AND enrichment_status = 'done'
        // safety guard in the SQL — if the track is not yet done, this is a no-op.
        self.track_repo
            .update_file_tags_written(msg.track_id, result.new_mtime, result.new_size_bytes)
            .await?;

        info!(
            track_id = %msg.track_id,
            correlation_id = %msg.correlation_id,
            "tag_writer: completed writeback"
        );

        // Non-fatal: a failed reindex is caught on the next startup rebuild.
        // Do not fail the enrichment pipeline or mark the track as failed.
        if let Err(e) = self.search_port.reindex_track(msg.track_id).await {
            warn!(
                track_id   = %msg.track_id,
                error      = %e,
                error.kind = %e.kind_str(),
                operation  = "search.reindex_track_failed",
                "Tantivy reindex failed after enrichment — will reconcile at next startup"
            );
        }

        Ok(())
    }
}

/// B2 fix: startup poller drains the entire backlog in a tight loop before
/// entering the normal interval-based polling mode. This ensures the
/// initial deployment burst (50,000 tracks from Pass 3 with NULL
/// tags_written_at) is processed in minutes, not days.
///
/// C4 note: if a tag write takes longer than poll_interval_secs, the same
/// track may be re-queued by the next poll cycle. This is safe (idempotent
/// write) but wastes SMB bandwidth. Consider adding a `tags_write_in_progress`
/// column if this is observed in production.
pub async fn run_startup_tag_poller(
    track_repo: Arc<dyn TrackRepository>,
    tag_writer_tx: mpsc::Sender<ToTagWriter>,
    poll_interval_secs: u64,
) {
    // Phase 1: drain the entire backlog on startup
    loop {
        let batch = track_repo
            .find_tags_unwritten(500)
            .await
            .unwrap_or_default();
        if batch.is_empty() {
            break;
        }
        info!(count = batch.len(), "tag_poller: draining startup backlog");
        for track in batch {
            let Some(blob_loc) = track.blob_location else {
                continue; // YouTube stubs — no file to write tags to
            };
            let _ = tag_writer_tx
                .send(ToTagWriter {
                    track_id: track.id,
                    blob_location: blob_loc,
                    correlation_id: uuid::Uuid::new_v4(),
                })
                .await;
        }
        // Small yield to avoid starving other tasks
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Phase 2: enter normal interval-based polling for newly done tracks
    let mut interval = tokio::time::interval(Duration::from_secs(poll_interval_secs));
    loop {
        interval.tick().await;

        match track_repo.find_tags_unwritten(200).await {
            Ok(tracks) => {
                let count = tracks.len();
                if count > 0 {
                    debug!(count = count, "tag_poller: found tracks pending writeback");
                }
                for track in tracks {
                    let Some(blob_loc) = track.blob_location else {
                        continue;
                    };
                    let _ = tag_writer_tx
                        .send(ToTagWriter {
                            track_id: track.id,
                            blob_location: blob_loc,
                            correlation_id: uuid::Uuid::new_v4(),
                        })
                        .await;
                }
            }
            Err(e) => {
                warn!(error = %e, "tag_poller: find_tags_unwritten error");
            }
        }
    }
}
