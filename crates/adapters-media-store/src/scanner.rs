use std::path::{Path, PathBuf};
use std::sync::Arc;

use application::ports::media_repository::MediaRepository;
use domain::error::DomainError;
use domain::media::{MediaAsset, MediaOrigin};
use tracing::{debug, error, info, trace};
use walkdir::WalkDir;

use crate::importer::compute_blake3_hash;

const SUPPORTED_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav", "aac", "m4a"];

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub total: usize,
    pub new: usize,
    pub skipped: usize,
    pub errors: usize,
}

impl std::fmt::Display for ScanReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Scanned {} files. {} new, {} skipped, {} errors.",
            self.total, self.new, self.skipped, self.errors
        )
    }
}

pub struct MediaScanner {
    media_root: PathBuf,
    repo: Arc<dyn MediaRepository>,
}

impl MediaScanner {
    pub fn new(media_root: impl Into<PathBuf>, repo: Arc<dyn MediaRepository>) -> Self {
        Self {
            media_root: media_root.into(),
            repo,
        }
    }

    pub async fn scan(&self) -> Result<ScanReport, DomainError> {
        let root = &self.media_root;
        if !root.exists() {
            return Err(DomainError::NotFound(format!(
                "Media root does not exist: {}",
                root.display()
            )));
        }

        info!(path = %root.display(), "Starting media scan");

        let mut total = 0usize;
        let mut new = 0usize;
        let mut skipped = 0usize;
        let mut errors = 0usize;

        // Collect paths synchronously (walkdir is not async)
        let paths: Vec<PathBuf> = WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
                    .unwrap_or(false)
            })
            .map(|entry| entry.into_path())
            .collect();

        for path in paths {
            total += 1;

            match self.process_file(&path).await {
                Ok(true) => new += 1,
                Ok(false) => {
                    trace!(file = %path.display(), "Already known, skipping");
                    skipped += 1;
                }
                Err(e) => {
                    error!(file = %path.display(), error = %e, "Failed to process file");
                    errors += 1;
                }
            }
        }

        let report = ScanReport { total, new, skipped, errors };
        info!(%report, "Media scan complete");
        Ok(report)
    }

    /// Process a single file. Returns `true` if a new asset was created, `false` if skipped.
    async fn process_file(&self, path: &Path) -> Result<bool, DomainError> {
        let content_hash = compute_blake3_hash(path)
            .await
            .map_err(|e| DomainError::InvalidState(format!("Hash failed for {}: {e}", path.display())))?;

        // Dedup check
        if self.repo.find_by_content_hash(&content_hash).await?.is_some() {
            return Ok(false);
        }

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown");

        // Title/artist heuristic: "Artist - Title" or whole stem as title
        let title = if let Some((_artist, title_part)) = stem.split_once(" - ") {
            title_part.trim().to_string()
        } else {
            stem.to_string()
        };

        let duration_ms = extract_duration(path);

        let abs_path = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string();

        let asset = MediaAsset {
            id: uuid::Uuid::new_v4(),
            title,
            origin: MediaOrigin::LocalManaged { rel_path: abs_path },
            duration_ms,
            content_hash: Some(content_hash),
            original_filename: Some(filename),
        };

        debug!(id = %asset.id, title = %asset.title, "Registering new media asset");
        self.repo.save(&asset).await?;
        Ok(true)
    }
}

/// Attempt to extract duration from an audio file using symphonia.
fn extract_duration(path: &Path) -> Option<u64> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probe = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;

    let track = probe.format.default_track()?;
    let time_base = track.codec_params.time_base?;
    let n_frames = track.codec_params.n_frames?;

    let time = time_base.calc_time(n_frames);
    let ms = (time.seconds as u64) * 1000 + (time.frac * 1000.0) as u64;
    Some(ms)
}
