const MAX_DECODE_SECS: u64 = 120;
use std::io::Cursor;
use std::path::Path;

use application::ports::enrichment::{AudioFingerprint, RawFileTags};
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey};

use application::error::AppError;

/// Decode the file at `path`, extract Chromaprint fingerprint and raw tags.
/// Reads first 120 seconds of PCM only. Returns (fingerprint, tags, duration_ms).
///
/// MUST be called from within spawn_blocking. Holds the SMB permit for its
/// entire duration — caller is responsible for acquiring it beforehand.
///
/// PERF-5: Reads the file into memory once, then shares the buffer between
/// lofty (tag extraction) and Symphonia (audio decode), eliminating the second
/// SMB file open that was required when each library opened the file independently.
pub fn read_file(path: &Path) -> Result<(AudioFingerprint, RawFileTags, u32), AppError> {
    // Single file read into memory — eliminates double SMB open.
    let file_bytes = std::fs::read(path).map_err(|e| AppError::Io {
        path: Some(path.to_owned()),
        source: e,
    })?;

    // --- lofty: read tags from in-memory buffer ---
    let mut cursor = Cursor::new(&file_bytes);
    let tagged_file = Probe::new(&mut cursor)
        .guess_file_type()
        .map_err(|e| AppError::TagRead {
            path: path.to_owned(),
            source: Box::new(e),
        })?
        .read()
        .map_err(|e| AppError::TagRead {
            path: path.to_owned(),
            source: Box::new(e),
        })?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    // Extract year via lofty's built-in date accessor.
    // Accessor::date() returns Option<Timestamp> where Timestamp.year is already
    // parsed as u16 from the correct tag key for each format (TDRC for ID3v2,
    // DATE for Vorbis comments, etc.) — no manual string parsing needed.
    let year: Option<i32> = tag
        .and_then(lofty::tag::Accessor::date)
        .map(|ts| i32::from(ts.year))
        .filter(|y| (1900..=2100).contains(y));

    let genres: Vec<String> = tag
        .map(|t| {
            t.items()
                .filter(|i| i.key() == ItemKey::Genre)
                .filter_map(|i| i.value().text().map(std::string::ToString::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut raw_tags = RawFileTags {
        title: tag.and_then(|t| t.title().map(std::borrow::Cow::into_owned)),
        artist: tag.and_then(|t| t.artist().map(std::borrow::Cow::into_owned)),
        album: tag.and_then(|t| t.album().map(std::borrow::Cow::into_owned)),
        year,
        genres: if genres.is_empty() {
            None
        } else {
            Some(genres)
        },
        track_number: tag.and_then(lofty::tag::Accessor::track),
        disc_number: tag.and_then(lofty::tag::Accessor::disk),
        bpm: tag
            .and_then(|t| t.get_string(ItemKey::Bpm))
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|&b| b > 0),
        isrc: tag.and_then(|t| {
            t.get_string(ItemKey::Isrc)
                .map(std::string::ToString::to_string)
        }),
        composer: tag.and_then(|t| {
            t.get_string(ItemKey::Composer)
                .map(std::string::ToString::to_string)
        }),
        lyricist: tag.and_then(|t| {
            t.get_string(ItemKey::Lyricist)
                .map(std::string::ToString::to_string)
        }),
        lyrics: tag.and_then(|t| {
            t.get_string(ItemKey::Lyrics)
                .map(std::string::ToString::to_string)
        }),
        bitrate: None,
        sample_rate: None,
        channels: None,
        codec: None,
        duration_ms: None, // filled from Symphonia below
    };

    // --- Symphonia: decode PCM for Chromaprint ---
    // Reuse the same in-memory buffer — no second file open needed.
    let cursor2 = Cursor::new(file_bytes);
    let mss = symphonia::core::io::MediaSourceStream::new(
        Box::new(cursor2),
        symphonia::core::io::MediaSourceStreamOptions::default(),
    );

    let probed = symphonia::default::get_probe()
        .format(
            &symphonia::core::probe::Hint::default(),
            mss,
            &symphonia::core::formats::FormatOptions::default(),
            &symphonia::core::meta::MetadataOptions::default(),
        )
        .map_err(|e| AppError::Fingerprint {
            path: path.to_owned(),
            source: Box::new(e),
        })?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| AppError::Fingerprint {
            path: path.to_owned(),
            source: Box::new(std::io::Error::other("no audio track found")),
        })?;

    let track_id = track.id;

    let duration_secs = track
        .codec_params
        .n_frames
        .zip(track.codec_params.time_base)
        .map_or(0.0, |(frames, tb)| {
            frames as f64 * f64::from(tb.numer) / f64::from(tb.denom)
        });

    let duration_secs_u32 = duration_secs as u32;

    let codec = symphonia::default::get_codecs()
        .get_codec(track.codec_params.codec)
        .map(|d| d.long_name.to_string());

    let bitrate = track.codec_params.bits_per_sample.map(|b| b as i32); // FLAC
    let sample_rate = track.codec_params.sample_rate.map(|s| s as i32);
    let channels = track.codec_params.channels.map(|c| c.count() as i32);

    raw_tags.bitrate = bitrate;
    raw_tags.sample_rate = sample_rate;
    raw_tags.channels = channels;
    raw_tags.codec = codec;

    let mut decoder = symphonia::default::get_codecs()
        .make(
            &track.codec_params,
            &symphonia::core::codecs::DecoderOptions::default(),
        )
        .map_err(|e| AppError::Fingerprint {
            path: path.to_owned(),
            source: Box::new(e),
        })?;

    let cp_sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let cp_channels = track.codec_params.channels.map_or(2, |c| c.count() as u16);

    let mut fp = chromaprint::Fingerprinter::new(chromaprint::Algorithm::default());
    let _ = fp.start(cp_sample_rate, cp_channels);

    let mut decoded_secs: f64 = 0.0;

    'decode: loop {
        if decoded_secs >= MAX_DECODE_SECS as f64 {
            break;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::ResetRequired) => break 'decode,
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let Ok(decoded_packet) = decoder.decode(&packet) else {
            continue;
        };

        let frames = decoded_packet.capacity();
        let add = frames as f64 / f64::from(cp_sample_rate);
        decoded_secs += add;

        // Convert to i16 samples for Chromaprint
        let spec = *decoded_packet.spec();
        let mut sample_buf = symphonia::core::audio::SampleBuffer::<i16>::new(frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded_packet);
        let samples = sample_buf.samples();

        let _ = fp.feed(samples);
    }

    let _ = fp.finish();
    // Use chromaprint's native encode() to produce the compressed base64
    // string expected by the AcoustID API (DESIGN-1 fix).
    let fingerprint_str = fp.encode();

    let duration_ms = if duration_secs > 0.0 {
        (duration_secs * 1000.0) as u32
    } else {
        (decoded_secs * 1000.0) as u32
    };

    raw_tags.duration_ms = Some(duration_ms);

    Ok((
        AudioFingerprint {
            fingerprint: fingerprint_str,
            duration_secs: duration_secs_u32.max((decoded_secs as u32).max(1)),
        },
        raw_tags,
        duration_ms,
    ))
}
