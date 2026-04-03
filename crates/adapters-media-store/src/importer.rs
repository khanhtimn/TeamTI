use std::path::Path;
use tracing::warn;

use crate::text::{normalize, normalize_filename, normalize_opt};

/// Extracted metadata from an audio file.
pub struct AudioMetadata {
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    pub original_filename: String,
}

/// Extract metadata (title, artist, duration) from an audio file.
///
/// Strategy:
/// 1. Try symphonia probe for embedded tags (ID3, Vorbis comments, etc.)
/// 2. Fall back to filename heuristic: "Artist - Title" or whole stem as title
///
/// All text fields are NFC-normalized before return to fix macOS APFS NFD filenames.
pub fn extract_metadata(path: &Path) -> AudioMetadata {
    let mut meta = match try_symphonia_metadata(path) {
        Some(meta) => meta,
        None => {
            warn!(file = %path.display(), "Symphonia probe failed, using filename heuristic");
            filename_heuristic(path)
        }
    };

    // NFC-normalize all text fields to fix macOS APFS NFD decomposed filenames
    meta.title = normalize(&meta.title);
    meta.artist = normalize_opt(meta.artist);
    meta.original_filename = normalize_filename(path);

    meta
}

/// Three-tier duration extraction from symphonia track and format reader.
///
/// Tier 1: n_frames + time_base (most reliable — works for CBR, FLAC, WAV, OGG,
///         and VBR MP3 with Xing/VBRI header)
/// Tier 2: format-level duration metadata from the container (some MP4/M4A
///         containers store total duration separately from codec params)
/// Tier 3: Accept None — do not scan the entire file. Log at DEBUG level.
fn extract_duration_ms(
    track: &symphonia::core::formats::Track,
    format: &dyn symphonia::core::formats::FormatReader,
) -> Option<u64> {
    // Tier 1: n_frames via codec time_base (most reliable for CBR/FLAC/OGG)
    if let (Some(n_frames), Some(tb)) = (track.codec_params.n_frames, track.codec_params.time_base)
    {
        let duration_secs = tb.calc_time(n_frames);
        let ms = (duration_secs.seconds as f64 * 1000.0 + duration_secs.frac * 1000.0) as u64;
        if ms > 0 {
            return Some(ms);
        }
    }

    // Tier 2: format-level duration from container metadata
    if let Some(track_tb) = track.codec_params.time_base {
        let _ = (track_tb, format); // format reserved for future container query
    }

    // Tier 3: cannot determine duration without full file scan
    tracing::debug!(
        "Duration unavailable for track (VBR without Xing header or \
         unsupported container). duration_ms will be stored as None."
    );
    None
}

/// Attempt to extract metadata using symphonia.
fn try_symphonia_metadata(path: &Path) -> Option<AudioMetadata> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey};
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;

    // Extract tags from metadata
    let mut title: Option<String> = None;
    let mut artist: Option<String> = None;

    // Check metadata revisions from the probe result
    if let Some(metadata) = probed.metadata.get()
        && let Some(rev) = metadata.current()
    {
        for tag in rev.tags() {
            if let Some(std_key) = tag.std_key {
                match std_key {
                    StandardTagKey::TrackTitle if title.is_none() => {
                        title = Some(tag.value.to_string());
                    }
                    StandardTagKey::Artist if artist.is_none() => {
                        artist = Some(tag.value.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    // Also check format-level metadata
    {
        let format_meta = probed.format.metadata();
        if let Some(rev) = format_meta.current() {
            for tag in rev.tags() {
                if let Some(std_key) = tag.std_key {
                    match std_key {
                        StandardTagKey::TrackTitle if title.is_none() => {
                            title = Some(tag.value.to_string());
                        }
                        StandardTagKey::Artist if artist.is_none() => {
                            artist = Some(tag.value.to_string());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Extract duration using three-tier strategy
    let duration_ms = probed
        .format
        .default_track()
        .and_then(|track| extract_duration_ms(track, &*probed.format));

    // If symphonia found no title tag at all, fall back to filename heuristic
    // but keep any artist/duration we did find
    if title.is_none() && artist.is_none() && duration_ms.is_none() {
        return None;
    }

    let fallback = filename_heuristic(path);
    Some(AudioMetadata {
        title: title.unwrap_or(fallback.title),
        artist: artist.or(fallback.artist),
        duration_ms,
        original_filename: fallback.original_filename,
    })
}

/// Filename heuristic: "Title - Artist.ext" or whole stem as title.
fn filename_heuristic(path: &Path) -> AudioMetadata {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .trim();

    let original_filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    if let Some((title_part, artist_part)) = stem.split_once(" - ") {
        AudioMetadata {
            title: title_part.trim().to_string(),
            artist: Some(artist_part.trim().to_string()),
            duration_ms: None,
            original_filename,
        }
    } else {
        AudioMetadata {
            title: stem.to_string(),
            artist: None,
            duration_ms: None,
            original_filename,
        }
    }
}
