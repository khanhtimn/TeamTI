════════════════════════════════════════════════════════════════════════════════
TEAMTI — NFD / UNICODE FIX + DURATION EXTRACTION PASS
Scope: fix Unicode normalization bugs and improve metadata extraction quality
════════════════════════════════════════════════════════════════════════════════

You are fixing two concrete bugs discovered from v1 runtime logs in the
`teamti` Rust workspace. Both bugs are isolated to `adapters-media-store`.
No other crates, crate boundaries, or architectural decisions change.

Read this entire document before writing any code.

════════════════════════════════════════════════════════════════════════════════
SECTION 1 — BUG 1: NFD UNICODE NORMALIZATION
════════════════════════════════════════════════════════════════════════════════

──────────────────────────────────────────────────────────────
1A. ROOT CAUSE
──────────────────────────────────────────────────────────────

macOS APFS returns filenames in NFD (Unicode Normalization Form D — decomposed).
Walkdir reads these filenames as-is. The result is that artist names and
original_filename values are stored in Postgres as NFD byte sequences.

Evidence from the v1 logs:
  artist=Some("pha\\u{301}t o\\u{31b}i tu\\u{31b}\\u{323} lo")
  path: "...pha\\u{301}t o\\u{31b}i tu\\u{31b}\\u{323} lo - Gio\\u{301} & AE.mp3"

"phát" should be stored as the single precomposed codepoint U+1EE3.
Instead it is stored as the base character 'a' followed by combining diacritics.

This causes:
  1. Discord autocomplete displays garbled or incorrectly rendered text
  2. Users typing "phát" in NFC send a different byte sequence than what is
     stored in Postgres — search matching is unreliable
  3. The content_hash is computed from the audio bytes (unaffected), but the
     filename-derived metadata is inconsistently normalized across platforms.
     If files are later moved to Linux (NFC filesystem), re-scanning the same
     audio file produces a different original_filename value even though the
     content_hash is identical.

──────────────────────────────────────────────────────────────
1B. AVAILABLE FIXES — READ BEFORE CHOOSING
──────────────────────────────────────────────────────────────

Three approaches exist. Evaluate all three and use the recommended one.

OPTION A — Rust-side NFC normalization (RECOMMENDED)
  Crate: unicode-normalization = "0.1"
  Usage: "your string".nfc().collect::<String>()
  The nfc() method implements UAX #15 NFC normalization.
  Apply at the point where filenames and tag strings enter the system,
  before any storage or hashing of text fields.

  Pros:
    - Normalization happens before data reaches Postgres
    - Content is clean in DB from the start
    - No Postgres function dependency
    - Works correctly even if the DB server encoding is not UTF8
    - The is_nfc_quick() check can short-circuit for strings already in NFC
      (most non-macOS content), making this near-zero cost for clean data

  Cons:
    - Requires a new crate dependency in adapters-media-store

OPTION B — Postgres NORMALIZE() function in generated columns
  Postgres 13+ has the SQL standard normalize() function:
    normalize(text, NFC)  -- converts to NFC
    text IS NFC NORMALIZED -- boolean check
  This could be used in the search_text generated column expression
  to normalize before indexing.

  Pros:
    - No application code change
    - Guaranteed at the DB layer

  Cons:
    - Only normalizes search_text and search_vector (the generated columns)
    - Does NOT fix title, artist, original_filename stored values
    - Discord still displays the raw NFD strings in autocomplete results
      because display values come from title/artist columns, not search_text
    - Requires server encoding = UTF8 (verify your Postgres instance)
    - Does not fix the cross-platform filename consistency problem
    - Solves only the search problem, not the display problem

  Verdict: insufficient on its own. May be used as a belt-and-suspenders
  addition to Option A, but not as the primary fix.

OPTION C — icu_normalizer crate (ICU4X)
  Crate: icu_normalizer
  More complete Unicode support, handles edge cases Option A does not.

  Pros:
    - More robust for rare combining character sequences

  Cons:
    - Heavy dependency with a large data bundle
    - unicode-normalization covers all practical music metadata cases
    - Significant compile time increase
    - Overkill for this use case

  Verdict: do not use.

CHOSEN APPROACH: Option A (Rust-side, unicode-normalization crate) plus
Option B as a belt-and-suspenders addition to the search_text generated column.

──────────────────────────────────────────────────────────────
1C. IMPLEMENTATION
──────────────────────────────────────────────────────────────

── Step 1: Add dependency ──

In crates/adapters-media-store/Cargo.toml:
  unicode-normalization = "0.1"

Do NOT add this to domain, application, or any other crate.
Unicode normalization is an infrastructure concern.

── Step 2: Create crates/adapters-media-store/src/text.rs ──

This module is the single source of truth for all text normalization
in the media store. Both importer.rs and scanner.rs use it.
Nothing else in the workspace imports it.

  use unicode_normalization::{UnicodeNormalization, is_nfc_quick, IsNormalized};

  /// Normalize a string to NFC form.
  /// Uses a quick pre-check to avoid allocation for already-normalized strings.
  /// This is a no-op for ASCII-only strings (is_nfc_quick returns Yes).
  pub(crate) fn normalize(s: &str) -> String {
      match is_nfc_quick(s.chars()) {
          IsNormalized::Yes => s.to_owned(),
          _ => s.nfc().collect(),
      }
  }

  /// Normalize an optional string. None passes through unchanged.
  pub(crate) fn normalize_opt(s: Option<String>) -> Option<String> {
      s.map(|v| normalize(&v))
  }

  /// Extract the filename stem from a path and normalize it to NFC.
  /// Falls back to empty string if the path has no filename.
  pub(crate) fn normalize_filename_stem(path: &std::path::Path) -> String {
      let stem = path
          .file_stem()
          .and_then(|s| s.to_str())
          .unwrap_or("");
      normalize(stem)
  }

  /// Extract the full filename (with extension) from a path, normalized to NFC.
  pub(crate) fn normalize_filename(path: &std::path::Path) -> String {
      let name = path
          .file_name()
          .and_then(|s| s.to_str())
          .unwrap_or("");
      normalize(name)
  }

Expose the module in crates/adapters-media-store/src/lib.rs as:
  mod text;
(not pub mod — this is internal to the crate)

── Step 3: Apply normalization in importer.rs ──

In the metadata extraction section of the importer, after extracting raw
tag strings from symphonia and after applying the filename heuristic fallback,
normalize ALL text fields before constructing the MediaAsset:

  use crate::text::{normalize, normalize_opt, normalize_filename};

  // After extracting raw strings from symphonia tags or filename heuristic:
  let title = normalize(&raw_title);
  let artist = normalize_opt(raw_artist);
  let original_filename = normalize_filename(source_path);

Do this BEFORE inserting into Postgres. The content_hash is computed from
audio bytes and is unaffected — do not normalize it.

── Step 4: Apply normalization in scanner.rs ──

The scanner calls the importer, so normalization happens inside the importer.
No additional normalization is needed in scanner.rs itself.
However, confirm that any log messages in scanner.rs that print metadata
are printing the normalized values (i.e., they print AFTER importer returns
the inserted MediaAsset, not before).

── Step 5: Belt-and-suspenders in migration 0003 (Optional but recommended) ──

Update the search_text generated column expression in migration 0003 to also
apply Postgres NFC normalization before lowercasing and unaccenting:

  -- Old expression:
  lower(unaccent(coalesce(title,'') || ' ' || coalesce(artist,'') || ...))

  -- New expression (Postgres normalize() requires UTF8 server encoding):
  lower(unaccent(
      normalize(coalesce(title,''), NFC)
      || ' ' || normalize(coalesce(artist,''), NFC)
      || ' ' || normalize(coalesce(original_filename,''), NFC)
  ))

IMPORTANT: This only applies to migration 0003 if your Postgres server
encoding is UTF8. Verify first with:
  SHOW server_encoding;
If it returns 'UTF8', apply this change. If not, skip this step and rely
on the Rust-side normalization only.

If you apply this change, create a new migration 0004 to ALTER and re-add
the generated column (Postgres does not allow ALTER on generated columns
in place — you must DROP and re-ADD). Do NOT modify migration 0003 itself.

Migration 0004 (only if Postgres encoding is UTF8):

  -- Re-create search_text with NFC normalization added
  ALTER TABLE media_assets DROP COLUMN IF EXISTS search_text;

  ALTER TABLE media_assets ADD COLUMN search_text TEXT
      GENERATED ALWAYS AS (
          lower(
              unaccent(
                  normalize(coalesce(title, ''), NFC)
                  || ' ' || normalize(coalesce(artist, ''), NFC)
                  || ' ' || normalize(coalesce(original_filename, ''), NFC)
              )
          )
      ) STORED;

  -- The trigram index must be recreated after the column is dropped and re-added
  DROP INDEX IF EXISTS idx_media_assets_trgm;
  CREATE INDEX idx_media_assets_trgm
      ON media_assets USING GIN(search_text gin_trgm_ops);

  -- Rebuild search_vector to also benefit from NFC normalization
  ALTER TABLE media_assets DROP COLUMN IF EXISTS search_vector;

  ALTER TABLE media_assets ADD COLUMN search_vector tsvector
      GENERATED ALWAYS AS (
          to_tsvector('music_simple',
              normalize(coalesce(title, ''), NFC)
              || ' ' || normalize(coalesce(artist, ''), NFC)
              || ' ' || normalize(coalesce(original_filename, ''), NFC)
          )
      ) STORED;

  DROP INDEX IF EXISTS idx_media_assets_search;
  CREATE INDEX idx_media_assets_search
      ON media_assets USING GIN(search_vector);

── Step 6: Backfill existing rows ──

Existing rows in the database have NFD values in title, artist, and
original_filename. They must be corrected. Add this to migration 0004
(or a standalone migration 0005 if you prefer to separate schema from data):

  -- Backfill NFC normalization on existing stored text fields
  -- Only needed when server encoding = UTF8
  UPDATE media_assets
  SET
      title            = normalize(title, NFC),
      artist           = normalize(artist, NFC),
      original_filename = normalize(original_filename, NFC)
  WHERE
      title             IS NFC NORMALIZED = false
      OR artist         IS NFC NORMALIZED = false
      OR original_filename IS NFC NORMALIZED = false;

If Postgres normalize() is not available (encoding not UTF8), perform this
backfill from the Rust side as a one-time migration script instead:
  - Query all rows with id, title, artist, original_filename
  - Normalize each string with text::normalize()
  - Update the row if any value changed

════════════════════════════════════════════════════════════════════════════════
SECTION 2 — BUG 2: DURATION EXTRACTION FOR VBR MP3
════════════════════════════════════════════════════════════════════════════════

──────────────────────────────────────────────────────────────
2A. ROOT CAUSE
──────────────────────────────────────────────────────────────

From the v1 logs:
  symphonia_bundle_mp3::demuxer: estimating duration from bitrate,
  may be inaccurate for vbr files

And the stored ResolvedPlayable shows: duration_ms: None

Symphonia sets n_frames = None when a VBR MP3 does not contain a Xing/VBRI
header (these headers, added by LAME and similar encoders, store the total
frame count). Without that header, Symphonia cannot determine the exact frame
count without scanning the entire file. This is an intentional design choice
in Symphonia — see github.com/pdeljanov/Symphonia issue #382.

The current importer only checks codec_params.n_frames. This is None for
VBR files without a Xing header, so duration_ms is always None for those files.

──────────────────────────────────────────────────────────────
2B. AVAILABLE APPROACHES — READ BEFORE CHOOSING
──────────────────────────────────────────────────────────────

APPROACH A — Three-tier extraction with TimeBase fallback (RECOMMENDED)
  Tier 1: codec_params.n_frames + codec_params.time_base (works for CBR,
          FLAC, WAV, OGG, and VBR MP3 with Xing header)
  Tier 2: format-level duration metadata from the container
          (some MP4/M4A containers store total duration separately)
  Tier 3: Accept None — do not scan the file. Log at DEBUG level.

APPROACH B — Full file scan for VBR MP3
  Decode every packet in the file just to count frames.
  This gives exact duration for all VBR MP3 files.

  Pros: accurate
  Cons: reads the entire audio file on every import — extremely slow for
        large files. A 10-minute MP3 takes 50-100ms to scan.
        For a startup scan of 1000 files, this adds 50-100 seconds.
        Do NOT use this approach.

APPROACH C — mp3-duration crate
  Crate: mp3-duration
  Specifically designed to estimate VBR MP3 duration by reading Xing/VBRI
  headers with a fast scan of just the first few frames, not the whole file.

  Pros: fast, focused, handles most VBR MP3s correctly
  Cons: MP3-specific (doesn't help FLAC/OGG/WAV). Adds a dependency.
        Would require combining with Approach A for non-MP3 files.

  Verdict: not worth the added dependency. Approach A covers the common cases
  that Approach C would fix, since VBR MP3s with Xing headers ARE handled by
  Symphonia's n_frames. The remaining VBR files without Xing headers are
  genuinely undeterminable without a full scan.

CHOSEN APPROACH: Approach A — three-tier extraction with format fallback.

──────────────────────────────────────────────────────────────
2C. IMPLEMENTATION
──────────────────────────────────────────────────────────────

In crates/adapters-media-store/src/importer.rs, replace the current duration
extraction logic with the following three-tier function:

  use symphonia::core::units::TimeBase;

  fn extract_duration_ms(
      track: &symphonia::core::formats::Track,
      format: &dyn symphonia::core::formats::FormatReader,
  ) -> Option<i64> {
      // Tier 1: n_frames via codec time_base (most reliable for CBR/FLAC/OGG)
      if let (Some(n_frames), Some(tb)) = (
          track.codec_params.n_frames,
          track.codec_params.time_base,
      ) {
          let duration_secs = tb.calc_time(n_frames);
          let ms = (duration_secs.seconds as f64 * 1000.0
              + duration_secs.frac * 1000.0) as i64;
          if ms > 0 {
              return Some(ms);
          }
      }

      // Tier 2: format-level duration from container metadata
      // Some MP4/M4A containers expose total duration separately from codec params
      if let Some(track_tb) = track.codec_params.time_base {
          // Duration may be stored at the format level in some containers
          // Symphonia exposes this via the track's time_base if n_frames is set
          // at the container level but not the codec level — check both
          let _ = (track_tb, format); // format reserved for future container query
      }

      // Tier 3: cannot determine duration without full file scan
      // Log at DEBUG, not WARN — this is expected for VBR MP3 without Xing header
      tracing::debug!(
          "Duration unavailable for track (VBR without Xing header or \
           unsupported container). duration_ms will be stored as None."
      );
      None
  }

Replace the current inline duration calculation call in the importer's
metadata extraction block with:
  let duration_ms = extract_duration_ms(track, &*format_reader);

Note the log level change: this must use tracing::debug!, NOT tracing::warn!.
The "estimating duration from bitrate" log is already emitted by Symphonia
itself. A second warning from your code creates duplicate noise. This is
expected behavior for VBR files, not a warning condition.

════════════════════════════════════════════════════════════════════════════════
SECTION 3 — WHAT NOT TO CHANGE
════════════════════════════════════════════════════════════════════════════════

  - Do not change any crate outside adapters-media-store (except Cargo.toml
    workspace dependencies if unicode-normalization needs to be added there)
  - Do not change domain, application, or adapters-discord
  - Do not change migrations 0001, 0002, or 0003
  - Do not change how BLAKE3 content_hash is computed
    (it is computed from audio bytes, not from text fields — it is correct)
  - Do not change the scanner's file extension filter or walkdir configuration
  - Do not add Symphonia file-scanning for VBR duration — it is too slow
  - Do not change the search query SQL

════════════════════════════════════════════════════════════════════════════════
SECTION 4 — VERIFICATION
════════════════════════════════════════════════════════════════════════════════

After implementing, verify by running the bot and checking:

  1. WIPE the existing DB rows inserted with NFD data:
       DELETE FROM media_assets;
     (Or run the backfill migration if you prefer to preserve them.)

  2. Restart the bot. The startup scan should re-import the 3 test files.

  3. Check the logs. Artist and title should now display as:
       artist=Some("phát ời tự lo")     ← precomposed NFC form
     NOT:
       artist=Some("pha\\u{301}t o\\u{31b}i tu\\u{31b}\\u{323} lo")

  4. Check that autocomplete for "phát" or "pha" returns results.
     The search now works because stored NFC matches user-typed NFC.

  5. Duration log should show tracing::debug! not tracing::warn! for VBR files.
     The "estimating duration from bitrate" line from Symphonia still appears —
     this is Symphonia's own log, not yours, and cannot be suppressed.

  6. cargo check --workspace must pass with zero errors.

  7. Run the idempotency check: restart the bot a second time without wiping
     the DB. The scan should report 0 new, 3 skipped, 0 errors.

════════════════════════════════════════════════════════════════════════════════
SECTION 5 — OUTPUT ORDER
════════════════════════════════════════════════════════════════════════════════

  1. Updated adapters-media-store/Cargo.toml
  2. New crates/adapters-media-store/src/text.rs
  3. Updated lib.rs (add mod text)
  4. Updated importer.rs (normalization + three-tier duration extraction)
  5. Confirmation that scanner.rs requires no changes (or diff if it does)
  6. Migration 0004 SQL (if Postgres encoding is UTF8)
     — re-created generated columns with normalize()
     — backfill UPDATE for existing rows
  7. Any changes to workspace Cargo.toml
  8. Verification log output showing correct NFC artist/title strings