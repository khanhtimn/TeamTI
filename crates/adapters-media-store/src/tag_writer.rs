use std::path::Path;

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, ItemValue, TagItem};

use application::ports::file_ops::{TagData, WriteResult};

/// Writes tags atomically:
///   1. Copy original to .{name}.{uuid}.tmp (same directory = same filesystem)
///   2. Open temp file with lofty, update tags, save_to_path(temp)
///   3. std::fs::rename(temp, original) — atomic on POSIX and SMB
///   4. stat(original) → new mtime + size
///
/// If any step fails, the temp file is cleaned up by TempGuard and an error
/// is returned. The original file is NEVER modified directly.
use application::error::{AppError, TagWriteErrorKind};

pub fn write_tags_atomic(path: &Path, tags: &TagData) -> Result<WriteResult, AppError> {
    let dir = path.parent().ok_or_else(|| AppError::TagWrite {
        path: path.to_owned(),
        kind: TagWriteErrorKind::LoftyError,
    })?;
    let filename = path
        .file_name()
        .ok_or_else(|| AppError::TagWrite {
            path: path.to_owned(),
            kind: TagWriteErrorKind::LoftyError,
        })?
        .to_string_lossy();

    // Temp file: hidden dot-prefix + UUID prefix → invisible to scanner,
    // but the final extension is preserved so `lofty` can infer the format properly.
    let temp_path = dir.join(format!(".tmp.{}.{}", uuid::Uuid::new_v4(), filename));

    // A2 fix: Guard must be a NAMED binding (not `let _ = ...`).
    // The guard must live until after rename succeeds or fails.
    let mut _temp_guard = TempGuard::new(&temp_path);

    // Step 1: Copy original → temp (preserves all audio data)
    std::fs::copy(path, &temp_path).map_err(|_| AppError::TagWrite {
        path: path.to_owned(),
        kind: TagWriteErrorKind::CopyFailed,
    })?;

    // Step 2: Open temp with lofty, modify tags
    {
        let mut tagged = Probe::open(&temp_path)
            .map_err(|e| {
                tracing::error!("lofty Probe::open failed: {:?}", e);
                AppError::TagWrite {
                    path: path.to_owned(),
                    kind: TagWriteErrorKind::LoftyError,
                }
            })?
            .read()
            .map_err(|e| {
                tracing::error!("lofty read failed: {:?}", e);
                AppError::TagWrite {
                    path: path.to_owned(),
                    kind: TagWriteErrorKind::LoftyError,
                }
            })?;

        // Use the primary tag if present; otherwise use the first available tag.
        // If no tag exists, we cannot write — skip gracefully.
        // Two separate checks to avoid simultaneous mutable borrows.
        let has_primary = tagged.primary_tag_mut().is_some();
        let tag = if has_primary {
            tagged.primary_tag_mut().unwrap()
        } else {
            tagged.first_tag_mut().ok_or_else(|| {
                tracing::error!("lofty found no valid tag format");
                AppError::TagWrite {
                    path: path.to_owned(),
                    kind: TagWriteErrorKind::NoTagFormat,
                }
            })?
        };

        tag.set_title(tags.title.clone());
        tag.set_artist(tags.artist.clone());

        if let Some(ref album) = tags.album_title {
            tag.set_album(album.clone());
        }
        // B4 fix: guard against negative or zero years.
        // lofty 0.23: use insert_text(ItemKey::Year) instead of removed set_year().
        if let Some(year) = tags.year
            && year > 0
        {
            tag.insert_text(ItemKey::Year, year.to_string());
        }

        // F1 fix: preserve multi-value genre shape.
        // remove_key clears all existing Genre items, then push() appends
        // one TagItem per genre. For FLAC/Vorbis this produces multiple
        // GENRE= comments; for ID3v2 lofty uses the first (per TCON spec).
        tag.remove_key(ItemKey::Genre);
        for genre in &tags.genres {
            tag.push(TagItem::new(ItemKey::Genre, ItemValue::Text(genre.clone())));
        }

        if let Some(track_num) = tags.track_number.and_then(|n| u32::try_from(n).ok()) {
            tag.set_track(track_num);
        }
        if let Some(disc_num) = tags.disc_number.and_then(|n| u32::try_from(n).ok()) {
            tag.set_disk(disc_num);
        }

        // F2 fix: write extended metadata fields.
        if let Some(bpm) = tags.bpm {
            tag.insert_text(ItemKey::Bpm, bpm.to_string());
        }
        if let Some(ref isrc) = tags.isrc {
            tag.insert_text(ItemKey::Isrc, isrc.clone());
        }
        if let Some(ref composer) = tags.composer {
            tag.insert_text(ItemKey::Composer, composer.clone());
        }
        if let Some(ref lyricist) = tags.lyricist {
            tag.insert_text(ItemKey::Lyricist, lyricist.clone());
        }
        if let Some(ref lyrics) = tags.lyrics {
            tag.insert_text(ItemKey::Lyrics, lyrics.clone());
        }

        // Save modified tags to the TEMP file (not the original)
        // lofty 0.23: save_to_path requires WriteOptions
        tagged
            .save_to_path(&temp_path, WriteOptions::default())
            .map_err(|e| {
                tracing::error!("lofty save_to_path failed: {:?}", e);
                AppError::TagWrite {
                    path: path.to_owned(),
                    kind: TagWriteErrorKind::LoftyError,
                }
            })?;
    }

    // Step 3: Atomic rename temp → original.
    // On the same filesystem (same SMB share), rename() is atomic.
    // If rename fails (e.g. cross-device), return error — temp cleaned by guard.
    // A2 fix: rename MUST succeed before disarm() is called.
    std::fs::rename(&temp_path, path).map_err(|e| {
        tracing::error!(
            "fs::rename failed from {:?} to {:?}: {:?}",
            temp_path,
            path,
            e
        );
        if e.kind() == std::io::ErrorKind::CrossesDevices {
            AppError::TagWrite {
                path: path.to_owned(),
                kind: TagWriteErrorKind::CrossDevice,
            }
        } else {
            AppError::TagWrite {
                path: path.to_owned(),
                kind: TagWriteErrorKind::LoftyError,
            }
        }
    })?;

    // Step 4: stat the original (now contains new tags)
    let meta = std::fs::metadata(path).map_err(|e| AppError::Io {
        path: Some(path.to_owned()),
        source: e,
    })?;
    let new_mtime: chrono::DateTime<chrono::Utc> = meta
        .modified()
        .map_err(|e| AppError::Io {
            path: Some(path.to_owned()),
            source: e,
        })?
        .into();
    let new_size_bytes = meta.len() as i64;

    // A2 fix: disarm AFTER rename succeeds — if rename had failed,
    // the guard would have dropped and removed the temp file.
    _temp_guard.disarm();

    Ok(WriteResult {
        new_mtime,
        new_size_bytes,
    })
}

/// RAII: removes the temp file on drop unless disarmed.
struct TempGuard {
    path: std::path::PathBuf,
    armed: bool,
}

impl TempGuard {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
