use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use adapters_watcher::{FileEvent, FileEventKind};
use application::ports::repository::TrackRepository;
use shared_config::Config;

/// Message sent from Classifier to Fingerprint Workers.
#[derive(Debug)]
pub struct ToFingerprint {
    pub path: PathBuf, // absolute path
    pub rel: String,
    pub mtime: SystemTime,
    pub size_bytes: u64,
    pub existing_id: Option<uuid::Uuid>,
    pub correlation_id: uuid::Uuid,
}

/// Supported audio extensions. Lowercase only — compare after to_lowercase().
pub static SUPPORTED_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav", "aac", "m4a", "opus"];

pub async fn run_classifier(
    config: Arc<Config>,
    track_repo: Arc<dyn TrackRepository>,
    mut file_rx: mpsc::Receiver<FileEvent>,
    fp_tx: mpsc::Sender<ToFingerprint>,
) {
    let supported: HashSet<&str> = SUPPORTED_EXTENSIONS.iter().copied().collect();
    let mut batch: Vec<FileEvent> = Vec::with_capacity(64);

    loop {
        batch.clear();
        let n = file_rx.recv_many(&mut batch, 64).await;
        if n == 0 {
            break; // channel closed
        }

        let removes: Vec<_> = batch
            .iter()
            .filter(|e| e.kind == FileEventKind::Remove)
            .collect();
        let creates: Vec<_> = batch
            .iter()
            .filter(|e| e.kind == FileEventKind::CreateOrModify)
            .collect();

        for e in &removes {
            let rel = relative_path(&config.media_root, &e.path);
            if let Err(err) = track_repo.mark_file_missing(&rel).await {
                warn!("classifier: mark_file_missing({rel}): {err}");
            } else {
                debug!("classifier: marked file_missing for {rel}");
            }
        }

        let supported_creates: Vec<_> = creates
            .into_iter()
            .filter(|e| {
                e.path
                    .extension()
                    .and_then(|ex| ex.to_str())
                    .map(|ex| ex.to_lowercase())
                    .map(|ex| supported.contains(ex.as_str()))
                    .unwrap_or(false)
            })
            .collect();

        // PERF-1: Use async stat to avoid blocking the tokio worker thread
        // on slow SMB/NFS mounts (stat can take 50-200ms per file).
        let mut stat_results: Vec<(&FileEvent, SystemTime, u64)> = Vec::new();
        for e in &supported_creates {
            if let Ok(meta) = tokio::fs::metadata(&e.path).await
                && let Ok(mtime) = meta.modified()
            {
                stat_results.push((*e, mtime, meta.len()));
            }
        }

        if stat_results.is_empty() {
            continue;
        }

        let rels: Vec<String> = stat_results
            .iter()
            .map(|(e, _, _)| relative_path(&config.media_root, &e.path))
            .collect();

        let existing_map = track_repo
            .find_many_by_blob_location(&rels)
            .await
            .unwrap_or_default();

        for (event, mtime, size_bytes) in stat_results {
            let rel = relative_path(&config.media_root, &event.path);
            let existing = existing_map.get(&rel);

            if let Some(track) = existing
                && let Some(db_mtime) = track.file_modified_at
            {
                let db_mt = SystemTime::from(db_mtime);
                let unchanged = db_mt
                    .duration_since(mtime)
                    .or_else(|_| mtime.duration_since(db_mt))
                    .map(|d| d.as_secs() < 2)
                    .unwrap_or(false)
                    && track.file_size_bytes == Some(size_bytes as i64);

                if unchanged {
                    debug!("classifier: skip unchanged {rel}");
                    continue;
                }
            }

            let correlation_id = uuid::Uuid::new_v4();
            let _ = fp_tx
                .send(ToFingerprint {
                    path: event.path.clone(),
                    rel,
                    mtime,
                    size_bytes,
                    existing_id: existing.map(|t| t.id),
                    correlation_id,
                })
                .await;
        }
    }
}

/// Convert absolute path to path relative to media_root.
/// Falls back to using path as-is if not under media_root.
pub fn relative_path(media_root: &Path, absolute: &Path) -> String {
    absolute
        .strip_prefix(media_root)
        .unwrap_or(absolute) // fallback: use as-is
        .to_string_lossy()
        .into_owned()
}
