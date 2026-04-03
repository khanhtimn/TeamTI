use std::fs::File;
use std::path::Path;

use application::ports::enrichment::{AudioFingerprint, RawFileTags};
use lofty::file::TaggedFileExt;
use lofty::tag::Accessor;

pub type TagReaderError = Box<dyn std::error::Error + Send + Sync>;

/// Decode the file at `path`, extract Chromaprint fingerprint and raw tags.
/// Reads first 120 seconds of PCM only. Returns (fingerprint, tags, duration_ms).
///
/// MUST be called from within spawn_blocking. Holds the SMB permit for its
/// entire duration — caller is responsible for acquiring it beforehand.
pub fn read_file(path: &Path) -> Result<(AudioFingerprint, RawFileTags, u32), TagReaderError> {
    // --- lofty: read tags (sequential, same file) ---
    let tagged_file = lofty::read_from_path(path)?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    // Extract year from tags — lofty 0.23 removed Accessor::year()
    let year: Option<i32> = tag.and_then(|t| {
        // Try TDRC/DATE tag values for year
        for item in t.items() {
            let val = item.value().text()?;
            // Parse YYYY from beginning of date string (e.g. "2024-01-15" or "2024")
            if val.len() >= 4
                && let Ok(y) = val[..4].parse::<i32>()
                && (1900..=2100).contains(&y)
            {
                return Some(y);
            }
        }
        None
    });

    let raw_tags = RawFileTags {
        title: tag.and_then(|t| t.title().map(|s| s.to_string())),
        artist: tag.and_then(|t| t.artist().map(|s| s.to_string())),
        album: tag.and_then(|t| t.album().map(|s| s.to_string())),
        year,
        genre: tag.and_then(|t| t.genre().map(|s| s.to_string())),
        track_number: tag.and_then(|t| t.track()),
        disc_number: tag.and_then(|t| t.disk()),
        duration_ms: None, // filled from Symphonia below
    };

    // --- Symphonia: decode PCM for Chromaprint ---
    // Two sequential file opens under one SMB permit:
    // 1. lofty::read_from_path — reads tag headers
    // 2. File::open → Symphonia decode — reads audio frames
    // Both are covered by the SMB permit held by the caller.
    // This is intentional: lofty does not expose its file handle
    // for reuse, so two opens are required.
    let file = File::open(path)?;
    let mss = symphonia::core::io::MediaSourceStream::new(Box::new(file), Default::default());

    let probed = symphonia::default::get_probe().format(
        &Default::default(),
        mss,
        &Default::default(),
        &Default::default(),
    )?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("no audio track found")?;

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.ok_or("no sample rate")?;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);

    let duration_secs = track
        .codec_params
        .n_frames
        .zip(track.codec_params.time_base)
        .map(|(frames, tb)| {
            let secs = frames as f64 * tb.numer as f64 / tb.denom as f64;
            secs as u32
        })
        .unwrap_or(0);

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &Default::default())?;

    let mut fp = chromaprint::Fingerprinter::new(chromaprint::Algorithm::default());
    let _ = fp.start(sample_rate, channels as u16);

    const MAX_DECODE_SECS: u64 = 120;
    let mut decoded_secs: f64 = 0.0;

    'decode: loop {
        if decoded_secs >= MAX_DECODE_SECS as f64 {
            break;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(symphonia::core::errors::Error::ResetRequired) => {
                // Treat as soft end-of-stream for fingerprinting purposes.
                // We have sufficient PCM; do not risk corrupting the fingerprint.
                break 'decode;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let frames = decoded.frames();
        decoded_secs += frames as f64 / sample_rate as f64;

        // Convert to i16 samples for Chromaprint
        let spec = *decoded.spec();
        let mut sample_buf = symphonia::core::audio::SampleBuffer::<i16>::new(frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();

        let _ = fp.feed(samples);
    }

    let _ = fp.finish();
    let raw_fp = fp.fingerprint();
    let fingerprint_str = raw_fp
        .iter()
        .map(|v| format!("{v:08x}"))
        .collect::<Vec<_>>()
        .join("");

    let duration_ms = if duration_secs > 0 {
        duration_secs * 1000
    } else {
        (decoded_secs * 1000.0) as u32
    };

    Ok((
        AudioFingerprint {
            fingerprint: fingerprint_str,
            duration_secs: duration_secs.max((decoded_secs as u32).max(1)),
        },
        RawFileTags {
            duration_ms: Some(duration_ms),
            ..raw_tags
        },
        duration_ms,
    ))
}
