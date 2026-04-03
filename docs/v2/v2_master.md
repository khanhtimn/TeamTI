# TeamTI v2 — Master Requirements & Design Document

> **Status:** Design locked. This is the single canonical reference for all v2
> implementation work. No design decision in this document may be revisited
> during implementation. Deviations require a separate design review.
> This document supersedes all prior architecture and pipeline documents.

---

## 1. Project Overview

TeamTI is a self-hosted Discord music bot written in Rust. It serves a private
Discord server, playing audio from a music library hosted on an SMB-mounted
NAS. The bot is not intended for public distribution.

### What v2 adds over v1

| Capability | v1 | v2 |
|---|---|---|
| Track identity | File path | Chromaprint audio fingerprint |
| Metadata source | File tags only | MusicBrainz via AcoustID enrichment |
| Artist model | Flat string | Normalized `artists` table, many-to-many with roles |
| Album model | Flat string | Normalized `albums` table |
| Cover art | None | Co-located `cover.jpg` + embedded in file tags |
| Change detection | None | mtime + file_size_bytes |
| File watching | Manual scan command | Automatic polling watcher (SMB-compatible) |
| Tag writeback | None | lofty writes enriched tags back to files atomically |
| Favorites | None | Global per Discord user ID, DB only |
| Listen history | None | Per-event, per-guild, DB only |
| Playlists | None | Owned by Discord user, DB only |
| Search | Trigram on filename | FTS + trigram on enriched title + artist |

### Explicitly out of scope for v2

- Web portal or HTTP API of any kind
- Public bot (multi-server verification, bot listing)
- Lyrics fetching or ReplayGain normalization
- Transcoding or format conversion
- `enrichment_locked` user command (DB-direct only in v2)
- Admin command to list untagged/exhausted tracks
- File organization / rename-to-canonical-structure command
- Any user-facing scan or enrichment status command

---

## 2. Non-Negotiable Design Principles

Every implementation decision must be consistent with all of these.

1. **Files and DB are both canonical, each for different data.** Files own
   audio metadata (title, artist, album, year, genre, track number, cover art).
   DB owns everything else (MBIDs, enrichment state, history, favorites,
   playlists, normalized relational data). Neither is a cache of the other.

2. **Chromaprint is the stable identity.** `audio_fingerprint` does not change
   when tags are written, when the file is renamed, or when it is moved. It
   changes only if the audio waveform itself changes. It is the primary
   deduplication key — not the file path, not any hash of the file bytes.

3. **mtime + file_size_bytes is the change detection mechanism.** BLAKE3 is
   not used anywhere in v2. It provides no benefit over mtime/size that
   Chromaprint does not already cover, and would require full file reads over
   SMB on every scan cycle.

4. **All file reads over SMB are gated by `SMB_READ_SEMAPHORE`.** The NAS must
   never receive more than `SMB_READ_CONCURRENCY` (default: 3) concurrent file
   read operations. This is a hard limit enforced by a shared `Arc<Semaphore>`.

5. **Only `done` tracks are visible to end users.** No pending, enriching,
   failed, low-confidence, unmatched, exhausted, or file-missing track ever
   appears in search results, autocomplete, or playback. No exceptions.

6. **The pipeline is fully automatic and hidden from users.** Scanning,
   enrichment, and tag writeback are invisible background operations with no
   user-facing progress indicators.

7. **Songbird's built-in `TrackQueue` is not used.** The bot calls
   `call.play_input()` directly and wires `TrackEvent` handlers manually.
   The guild queue stores `VecDeque<(Uuid, TrackHandle)>` pairs — UUID and
   handle are always inserted and removed together.

8. **`PollWatcher` is the only watcher backend.** `INotifyWatcher`,
   `FsEventWatcher`, and `RecommendedWatcher` are never used. SMB mounts do
   not support OS-level filesystem notifications on any platform.

9. **A scan must never overlap with a previous scan.** An `AtomicBool`
   `scan_in_progress` flag prevents concurrent poll cycles regardless of
   interval setting.

---

## 3. Architecture — Split Authority Model

### Data ownership

| Data field | File tags (lofty) | Database | Notes |
|---|---|---|---|
| `title` | ✅ written after enrichment | ✅ | DB is primary; file is synchronized copy |
| `artist` display string | ✅ written after enrichment | ✅ `artist_display` | Denormalized for search |
| `album` | ✅ written after enrichment | ✅ FK to `albums` | |
| `year` | ✅ | ✅ | |
| `track_number`, `disc_number` | ✅ | ✅ | |
| `genre` | ✅ | ✅ | |
| Embedded cover art | ✅ bytes embedded | path in `albums.cover_art_path` | |
| MusicBrainz Recording ID | ❌ | ✅ `tracks.mbid` | DB only |
| AcoustID fingerprint | ❌ | ✅ `tracks.audio_fingerprint` | |
| AcoustID track ID | ❌ | ✅ `tracks.acoustid_id` | |
| Enrichment state | ❌ | ✅ | Internal pipeline state |
| Listen history | ❌ | ✅ `listen_events` | |
| Favorites | ❌ | ✅ `favorites` | |
| Playlists | ❌ | ✅ `playlists`, `playlist_items` | |

### Source of truth on conflict

When a file is rescanned and already has `enrichment_status = 'done'` in the
DB, the DB wins for all fields. File tags can only update the DB if an admin
runs `/rescan --force`, which resets `enrichment_status = 'pending'` for
eligible tracks.

---

## 4. Complete Database Schema

### Prerequisites — migration 0001

```sql
-- Required before any table that uses immutable_unaccent() in generated
-- columns or functional indexes.
CREATE EXTENSION IF NOT EXISTS unaccent;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Standard unaccent() is STABLE, not IMMUTABLE.
-- Generated columns require IMMUTABLE. This wrapper is mandatory.
-- Without it, migration 0002 will fail with:
--   ERROR: generation expression is not immutable
CREATE OR REPLACE FUNCTION immutable_unaccent(text)
  RETURNS text LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
  $$ SELECT unaccent($1) $$;

-- No stemming, unaccent + lowercase only.
-- Stemming corrupts artist and album names.
CREATE TEXT SEARCH CONFIGURATION music_simple (COPY = simple);
ALTER TEXT SEARCH CONFIGURATION music_simple
  ALTER MAPPING FOR word, hword, hword_part
  WITH unaccent, simple;
```

### Core tables — migration 0002

```sql
CREATE TABLE artists (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    sort_name   TEXT NOT NULL,        -- "Beatles, The" for sort
    mbid        TEXT UNIQUE,
    country     TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE albums (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title           TEXT NOT NULL,
    release_year    INTEGER,
    total_tracks    INTEGER,
    total_discs     INTEGER DEFAULT 1,
    mbid            TEXT UNIQUE,      -- MusicBrainz Release ID
    cover_art_path  TEXT,             -- relative to MEDIA_ROOT
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- role: 'primary' | 'various' | 'compiler'
CREATE TABLE album_artists (
    album_id    UUID NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    artist_id   UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'primary',
    position    INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (album_id, artist_id)
);

CREATE TABLE tracks (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Audio metadata (synchronized to file tags after enrichment)
    title           TEXT NOT NULL,
    artist_display  TEXT,             -- denormalized; primary artist for search/display
    album_id        UUID REFERENCES albums(id),
    track_number    INTEGER,
    disc_number     INTEGER DEFAULT 1,
    duration_ms     INTEGER,
    genre           TEXT,
    year            INTEGER,

    -- File identity and change detection
    -- NOTE: No BLAKE3 / file_hash column. mtime + size is sufficient.
    audio_fingerprint   TEXT,         -- Chromaprint; primary dedup key
    file_modified_at    TIMESTAMPTZ,  -- mtime; change detection
    file_size_bytes     BIGINT,       -- file size in bytes; change detection
    blob_location       TEXT NOT NULL, -- relative to MEDIA_ROOT

    -- Enrichment pipeline state
    mbid                    TEXT,
    acoustid_id             TEXT,
    enrichment_status       TEXT NOT NULL DEFAULT 'pending',
    enrichment_confidence   REAL,         -- 0.0–1.0
    enrichment_attempts     INTEGER NOT NULL DEFAULT 0,
    enrichment_locked       BOOLEAN NOT NULL DEFAULT false,
    enriched_at             TIMESTAMPTZ,

    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Generated search columns
    search_text TEXT GENERATED ALWAYS AS (
        lower(immutable_unaccent(
            normalize(coalesce(title, ''), NFC) || ' ' ||
            normalize(coalesce(artist_display, ''), NFC) || ' ' ||
            normalize(coalesce(genre, ''), NFC)
        ))
    ) STORED,

    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('music_simple',
            normalize(coalesce(title, ''), NFC) || ' ' ||
            normalize(coalesce(artist_display, ''), NFC)
        )
    ) STORED
);

-- role: 'primary' | 'featuring' | 'remixer' | 'producer'
CREATE TABLE track_artists (
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    artist_id   UUID NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'primary',
    position    INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (track_id, artist_id, role)
);
```

### User library tables — migration 0003

```sql
-- Favorites: global per Discord user ID (not guild-scoped)
CREATE TABLE favorites (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     TEXT NOT NULL,        -- Discord snowflake as string
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, track_id)
);

-- One row per play event
CREATE TABLE listen_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     TEXT NOT NULL,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    guild_id    TEXT NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed   BOOLEAN NOT NULL DEFAULT false
    -- true: played to natural end
    -- false: skipped or interrupted
);

CREATE TABLE playlists (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    owner_id    TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE playlist_items (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    playlist_id     UUID NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id        UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    added_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (playlist_id, position)
);
```

### Indexes — migration 0004

```sql
CREATE UNIQUE INDEX idx_tracks_fingerprint
    ON tracks(audio_fingerprint)
    WHERE audio_fingerprint IS NOT NULL;

CREATE INDEX idx_tracks_blob_location  ON tracks(blob_location);
CREATE INDEX idx_tracks_search_vector  ON tracks USING GIN(search_vector);
CREATE INDEX idx_tracks_search_text    ON tracks USING GIN(search_text gin_trgm_ops);

CREATE INDEX idx_tracks_enrichment_queue
    ON tracks(enrichment_status, enrichment_attempts, enriched_at)
    WHERE enrichment_locked = false
      AND enrichment_status IN ('pending', 'failed', 'low_confidence', 'unmatched');

CREATE INDEX idx_favorites_user      ON favorites(user_id);
CREATE INDEX idx_listen_events_user  ON listen_events(user_id, started_at DESC);
CREATE INDEX idx_listen_events_track ON listen_events(track_id);
```

---

## 5. Enrichment Status State Machine

┌─────────────────┐
INSERT │ │
──────────────► │ pending │ ◄── /rescan --force resets exhausted
│ │
└────────┬────────┘
│ worker claims (FOR UPDATE SKIP LOCKED)
▼
┌─────────────────┐
│ enriching │ stale: reset to pending after 10m
└──────┬──────────┘
AcoustID │
┌──────────────────┼──────────────────┐
│ │ │
score ≥ 0.85 score < 0.85 no results
│ │ │
▼ ▼ ▼
(continues to low_confidence unmatched
MusicBrainz) │ │
│ attempts < attempts <
│ FAILED_RETRY UNMATCHED_RETRY
│ → retry in 1h → retry in 24h
│ │ │
│ attempts ≥ attempts ≥
│ limit limit
│ └──────┬──────────────┘
│ ▼
│ ┌───────────────┐
│ │ exhausted │ invisible to users
│ └───────────────┘
│
MusicBrainz fetch + upsert
Cover art fetch
lofty tag writeback (atomic)
stat() for new mtime/size
│
▼
┌───────────────┐ ┌───────────────┐
│ done │ │ failed │ network error; retry in 1h
└───────────────┘ └───────────────┘

file_missing: set when PollWatcher fires Remove, or blob_location no longer
found during scan. Preserves all history, favorites, playlists.

text

### Retry matrix

| Status | Retried | Condition | On limit |
|---|---|---|---|
| `pending` | Always | — | — |
| `failed` | Yes | `attempts < FAILED_RETRY_LIMIT` AND `enriched_at < now() - 1h` | → `exhausted` |
| `low_confidence` | Yes | `attempts < FAILED_RETRY_LIMIT` AND `enriched_at < now() - 1h` | → `exhausted` |
| `unmatched` | Yes | `attempts < UNMATCHED_RETRY_LIMIT` AND `enriched_at < now() - 24h` | → `exhausted` |
| `exhausted` | **Never** | — | Reset only via `/rescan --force` |
| `done` | **Never** | — | — |
| `enriching` | No | Stale watchdog resets after 10m | |
| `file_missing` | No | — | — |

---

## 6. Track Visibility Model

All search, autocomplete, and playback commands enforce this table.
No exceptions in v2.

| `enrichment_status` | Visible in `/search` | Playable |
|---|---|---|
| `pending` | ❌ | ❌ |
| `enriching` | ❌ | ❌ |
| `done` | ✅ | ✅ |
| `low_confidence` | ❌ | ❌ |
| `unmatched` | ❌ | ❌ |
| `failed` | ❌ | ❌ |
| `exhausted` | ❌ | ❌ |
| `file_missing` | ❌ | ❌ |

---

## 7. File System Layout

MEDIA_ROOT/
├── {ArtistSortName}/
│ ├── folder.jpg ← artist portrait (optional)
│ └── {AlbumTitle} ({Year})/
│ ├── cover.jpg ← album art; resolved in order:
│ │ 1. pre-existing cover.jpg in directory
│ │ 2. fetched from Cover Art Archive /front-500
│ │ 3. extracted from file's embedded tags
│ │ 4. absent (cover_art_path = NULL)
│ ├── 01 - {TrackTitle}.mp3
│ └── 02 - {TrackTitle}.flac
└── Unsorted/
└── {original_filename}.ext ← pre-organization; fully valid

text

**Supported extensions:** `mp3`, `flac`, `ogg`, `wav`, `aac`, `m4a`, `opus`.
All other extensions are silently skipped.

`blob_location` is always stored **relative to `MEDIA_ROOT`**.
Example: `"The Beatles/Abbey Road (1969)/01 - Come Together.mp3"`

**File moves and renames are supported.** The next poll fires `Remove` on
the old path and `Create` on the new path. The Fingerprint Worker finds the
same `audio_fingerprint` in the DB and updates `blob_location`. Identity,
history, favorites, and playlists are all preserved.

**Duplicate fingerprints** (same audio at two paths simultaneously):
first-indexed path wins. The second is logged as a warning and discarded.
If the first path becomes `file_missing`, the second is picked up on the
next poll as a new `Create` event.

---

## 8. Background Pipeline

### Watcher

- **Backend:** `notify::PollWatcher` only. Never `RecommendedWatcher`.
- **Wrapper:** `notify-debouncer-full` via `new_debouncer_opt::<_, PollWatcher>`
- **Poll interval:** `SCAN_INTERVAL_SECS` (default: **300s**)
- **Debounce window:** 5s (absorbs chunked writes over SMB)
- **Runs on:** `std::thread` (not tokio task); bridges via `tx.blocking_send()`
- **Overlap guard:** `AtomicBool scan_in_progress` (AcqRel/Acquire ordering).
  If the flag is set when a poll tick fires, the tick is skipped entirely.
- **Events emitted:** `Create`, `Modify`, `Remove`. `Access` is ignored.

### Full pipeline

PollWatcher (std::thread, overlap-guarded)
│ blocking_send FileEvent { path, kind }
│ mpsc capacity: 2048
▼
Classifier (single tokio task)
│ mtime + size check (stat only, no file read)
│ mpsc capacity: 256
▼
Fingerprint Worker (spawn_blocking, ≤FINGERPRINT_CONCURRENCY concurrent)
│ One SMB read: Symphonia PCM decode + chromaprint-next + lofty tag read
│ DB lookup by fingerprint → insert or update
│ mpsc capacity: 128
▼
Enrichment Orchestrator (single tokio task)
│ polls DB every SCAN_INTERVAL_SECS for pending/failed/etc.
│ FOR UPDATE SKIP LOCKED, batch 50
│ mpsc capacity: 64
▼
AcoustID Worker (single tokio task, governor 1 req/sec)
│ mpsc capacity: 64
▼
MusicBrainz Worker (single tokio task, governor 1 req/sec)
│ tokio::spawn fan-out
├──► Cover Art Fetcher (Arc<Semaphore> COVER_ART_CONCURRENCY permits)
└──► Tag Writer (spawn_blocking per track)

text

### Classifier algorithm

Remove event:
UPDATE tracks SET enrichment_status = 'file_missing'
WHERE blob_location = relative(path)
→ done

Create / Modify event:
1. Check extension. Skip if not in supported set.

2. stat(path) → mtime, size_bytes
(metadata-only; no file bytes; no SMB_READ_SEMAPHORE)

3. SELECT id, file_modified_at, file_size_bytes
FROM tracks WHERE blob_location = relative(path)

IF found AND file_modified_at == mtime AND file_size_bytes == size:
→ SKIP (unchanged; fastest path; zero file reads)

4. Emit ToFingerprint { path, mtime, size_bytes, existing_id: Option<Uuid> }
(No BLAKE3 computation at any point in the pipeline)

text

### Fingerprint Worker algorithm

acquire SMB_READ_SEMAPHORE
spawn_blocking:
// Single SMB read pass — three purposes served simultaneously:
// Symphonia: decode first 120s PCM → chromaprint-next fingerprint
// lofty: read existing tags + audio properties (duration_ms)
(audio_fingerprint, raw_tags, duration_ms)
release SMB_READ_SEMAPHORE

SELECT id, blob_location, enrichment_status
FROM tracks WHERE audio_fingerprint = $fp

CASE: fingerprint found, blob_location matches or existing_id matches
→ Same audio, tags may have changed (e.g. enrichment writeback by us)
UPDATE file_modified_at = mtime, file_size_bytes = size_bytes
Leave enrichment_status unchanged
done

CASE: fingerprint found, blob_location differs
→ Same audio, file was moved or renamed
UPDATE blob_location, file_modified_at, file_size_bytes
Leave enrichment_status unchanged
done

CASE: no fingerprint match
→ New audio content
INSERT tracks (title, artist_display, duration_ms, blob_location,
audio_fingerprint, file_modified_at, file_size_bytes,
enrichment_status = 'pending', enrichment_attempts = 0)
Emit ToEnrichment { track_id }

text

### Enrichment Orchestrator claim query

```sql
SELECT id FROM tracks
WHERE enrichment_locked = false
  AND (
    (enrichment_status IN ('pending', 'failed', 'low_confidence')
     AND enrichment_attempts < $failed_retry_limit
     AND (enriched_at IS NULL OR enriched_at < now() - INTERVAL '1 hour'))
    OR
    (enrichment_status = 'unmatched'
     AND enrichment_attempts < $unmatched_retry_limit
     AND enriched_at < now() - INTERVAL '24 hours')
  )
ORDER BY created_at ASC
LIMIT 50
FOR UPDATE SKIP LOCKED
```

### AcoustID Worker

governor.until_ready().await (zero busy-wait; backpressures upstream)

POST https://api.acoustid.org/v2/lookup
client={ACOUSTID_API_KEY}
fingerprint={audio_fingerprint}
duration={duration_ms / 1000}
meta=recordings+releasegroups+compress

score >= ENRICHMENT_CONFIDENCE_THRESHOLD:
UPDATE enrichment_confidence, acoustid_id
Emit ToMusicBrainz { track_id, recording_mbid, confidence }

score < threshold (weak match):
UPDATE enrichment_status = 'low_confidence', enrichment_attempts += 1
IF attempts >= FAILED_RETRY_LIMIT: UPDATE enrichment_status = 'exhausted'

No results:
UPDATE enrichment_status = 'unmatched', enrichment_attempts += 1
IF attempts >= UNMATCHED_RETRY_LIMIT: UPDATE enrichment_status = 'exhausted'

Network/HTTP error:
UPDATE enrichment_status = 'failed', enrichment_attempts += 1
IF attempts >= FAILED_RETRY_LIMIT: UPDATE enrichment_status = 'exhausted'

text

### MusicBrainz Worker

governor.until_ready().await (separate 1 req/sec instance from AcoustID)

GET https://musicbrainz.org/ws/2/recording/{mbid}
?inc=releases+release-groups+artists+genres
Accept: application/json
User-Agent: {MB_USER_AGENT}

Parse: title, artist credits, release, year, genre
Upsert Artist rows by MBID (INSERT ... ON CONFLICT (mbid) DO UPDATE)
Upsert Album row by release MBID
Upsert TrackArtist, AlbumArtist join rows (ON CONFLICT DO NOTHING)
UPDATE tracks: title, artist_display, album_id, genre, year, mbid, enriched_at
(enrichment_status stays 'enriching' until tag write completes)

Emit ToFanOut { track_id, release_mbid, blob_location, metadata }

text

### Atomic tag writeback

    acquire SMB_READ_SEMAPHORE

    lofty::read_from_path(absolute_path)

    Apply enriched fields: title, artist, album, year, genre, track#, disc#

    Embed cover art bytes (if cover.jpg was fetched)

    lofty::save_to_path(tempfile in same directory as original)

    std::fs::rename(tempfile, original_path) ← atomic on POSIX/CIFS

    stat(original_path) → new_mtime, new_size
    (metadata call only; semaphore still held; no extra file read)

    release SMB_READ_SEMAPHORE

    UPDATE tracks SET:
    file_modified_at = new_mtime
    file_size_bytes = new_size
    enrichment_status = 'done'
    enriched_at = now()

text

`std::fs::rename` on Linux CIFS is atomic for same-share renames.
Durability (fsync) is not guaranteed on all NAS firmware. This is an accepted
risk for a music tagger — worst case is a corrupted tag on crash, not data loss.

### Cover Art Fetcher

Resolution order (first success wins):
1. cover.jpg already exists in album directory → use it, skip fetch
2. GET https://coverartarchive.org/release/{release_mbid}/front-500
3. Extract embedded art from file tags via lofty (AttachedPictureFrame)
4. Absent → cover_art_path = NULL

Save as: {album_dir}/cover.jpg (relative path stored in albums.cover_art_path)
Cover art stored at 500px as-is (no resizing in v2)
Concurrency limited by Arc<Semaphore> COVER_ART_CONCURRENCY permits

text

### Concurrency controls summary

| Resource | Mechanism | Limit |
|---|---|---|
| SMB file reads (Symphonia, lofty read/write) | `Arc<Semaphore>` | `SMB_READ_CONCURRENCY` (default 3) |
| Fingerprint Workers in flight | `Arc<Semaphore>` | `FINGERPRINT_CONCURRENCY` (default 4) |
| AcoustID HTTP | `governor` GCRA | 1 req/sec |
| MusicBrainz HTTP | `governor` GCRA | 1 req/sec (separate instance) |
| Cover Art Archive HTTP | `Arc<Semaphore>` | `COVER_ART_CONCURRENCY` (default 4) |

---

## 9. Queue Model

```rust
struct GuildQueue {
    // UUID and TrackHandle are always inserted and removed as a pair.
    // They are never manipulated independently.
    // This eliminates all possibility of positional desync.
    items:          VecDeque<(Uuid, TrackHandle)>,
    current:        Option<(Uuid, TrackHandle)>,
    skip_requested: bool,
    notify_channel: Option<ChannelId>,
}
// Stored in Serenity TypeMap: HashMap<GuildId, GuildQueue>
```

Songbird's `TrackQueue` is **not used**. The bot calls `call.play_input()`
directly and wires `TrackEvent` handlers manually.

### Playback advance (on TrackEvent::End or TrackEvent::Error)

    Lock GuildQueue for this GuildId

    Write listen_events:
    completed = (event == End) AND (NOT skip_requested)

    current = None; skip_requested = false

    Pop front of items → next: Option<(Uuid, TrackHandle)>

    IF next.is_some():
    Verify Uuid still has enrichment_status = 'done'
    Verify blob_location exists (stat() check; no file read)
    IF valid:
    absolute = config.media_root.join(&track.blob_location)
    call.play_input(absolute) → set current = next
    IF invalid:
    send ephemeral to notify_channel ("Track unavailable, skipping")
    repeat from step 4

    IF items is empty: remain connected but idle

text

### On voice gateway error or WebSocket disconnect

    Lock GuildQueue

    Write listen_events for current with completed = false

    current = None; skip_requested = false

    items.clear() ← drain entirely; queue cannot resume across broken connection

    Send message to notify_channel: "Voice connection lost, queue cleared"

text

---

## 10. Discord Commands

All commands use slash syntax. All search/autocomplete queries add
`WHERE enrichment_status = 'done'`.

### Playback (all users)

| Command | Behaviour |
|---|---|
| `/play <query>` | Autocomplete from `done` tracks; add to queue or play immediately if idle; join voice if needed |
| `/pause` | Pause current track |
| `/resume` | Resume paused track |
| `/skip` | Set `skip_requested = true`; advance queue |
| `/stop` | Stop playback, drain queue, leave voice channel |
| `/queue` | Display current queue (title + artist_display) |
| `/nowplaying` | Display current track with album and duration |

### Admin (ADMINISTRATOR permission only)

| Command | Behaviour |
|---|---|
| `/rescan --force` | Reset `exhausted` and `low_confidence` tracks to `pending`; does not touch `done` or `enrichment_locked = true` tracks |

---

## 11. Crate Map

teamti/
├── crates/
│ ├── domain/
│ │ Entities: Track, Artist, Album, TrackArtist, AlbumArtist,
│ │ Favorite, ListenEvent, Playlist, PlaylistItem.
│ │ Enums: EnrichmentStatus, ArtistRole.
│ │ No dependencies except uuid, chrono, serde.
│ │
│ ├── application/
│ │ Ports (traits only — no adapter imports):
│ │ TrackRepository, ArtistRepository, AlbumRepository
│ │ TrackSearchPort
│ │ FingerprintPort
│ │ AcoustIdPort
│ │ MusicBrainzPort
│ │ CoverArtPort
│ │ FileTagWriterPort
│ │ LibraryQueryPort
│ │ Use cases:
│ │ EnrichmentOrchestrator
│ │ PlaybackService
│ │
│ ├── shared-config/
│ │ All env vars parsed once into Arc<Config>.
│ │ See §12 for full reference.
│ │
│ ├── shared-observability/
│ │ tracing + tracing-subscriber. Unchanged from v1.
│ │
│ ├── adapters-discord/
│ │ Serenity command handlers for all commands in §10.
│ │ Autocomplete: TrackSearchPort.
│ │ No business logic — delegates to application layer only.
│ │
│ ├── adapters-voice/
│ │ Songbird integration.
│ │ GuildQueue in TypeMap (HashMap<GuildId, GuildQueue>).
│ │ Manual call.play_input() + TrackEvent wiring.
│ │ Path resolution: config.media_root.join(&track.blob_location).
│ │ listen_events writes on TrackEvent::End and ::Error.
│ │
│ ├── adapters-persistence/
│ │ sqlx::PgPool implementations of:
│ │ TrackRepository, ArtistRepository, AlbumRepository
│ │ TrackSearchPort (FTS + trigram hybrid)
│ │ LibraryQueryPort
│ │ All enrichment worker DB queries live here.
│ │
│ ├── adapters-media-store/
│ │ Scanner: PollWatcher → Classifier → Fingerprint Worker.
│ │ Constructs SMB_READ_SEMAPHORE (Arc<Semaphore>), shared with tag writer.
│ │ lofty: tag reading and atomic tag writeback (FileTagWriterPort impl).
│ │ Symphonia: PCM decode for Chromaprint.
│ │ chromaprint-next (features = ["parallel"]): fingerprint computation.
│ │ No BLAKE3 anywhere in this crate.
│ │
│ ├── adapters-watcher/
│ │ notify::PollWatcher via new_debouncer_opt::<_, PollWatcher>.
│ │ AtomicBool scan_in_progress overlap guard.
│ │ std::thread bridge to Tokio via blocking_send.
│ │ Emits FileEvent { path: PathBuf, kind: FileEventKind }.
│ │
│ ├── adapters-acoustid/
│ │ reqwest HTTP client.
│ │ governor GCRA 1 req/sec.
│ │ Implements AcoustIdPort.
│ │
│ ├── adapters-musicbrainz/
│ │ musicbrainz_rs async (no "blocking" feature).
│ │ governor GCRA 1 req/sec (separate instance from AcoustID).
│ │ Upserts Artist, Album, join tables.
│ │ Implements MusicBrainzPort.
│ │
│ └── adapters-cover-art/
│ reqwest HTTP client.
│ Arc<Semaphore> COVER_ART_CONCURRENCY permits.
│ lofty embedded art extraction fallback.
│ Saves cover.jpg co-located with tracks.
│ Implements CoverArtPort.
│
└── apps/
└── bot/
Startup sequence (§13).
Wires all adapters to ports via Arc<dyn Port>.
Constructs SMB_READ_SEMAPHORE and CancellationToken.

text

---

## 12. Configuration Reference

| Variable | Type | Default | Description |
|---|---|---|---|
| `DATABASE_URL` | String | **required** | PostgreSQL connection string |
| `DISCORD_TOKEN` | String | **required** | Discord bot token |
| `ACOUSTID_API_KEY` | String | **required** | AcoustID application key |
| `MB_USER_AGENT` | String | **required** | e.g. `"TeamTI/0.2.0 (you@email.com)"` |
| `MEDIA_ROOT` | Path | **required** | Absolute path to SMB music directory |
| `SCAN_INTERVAL_SECS` | u64 | `300` | PollWatcher poll interval |
| `SMB_READ_CONCURRENCY` | usize | `3` | SMB_READ_SEMAPHORE permits |
| `FINGERPRINT_CONCURRENCY` | usize | `4` | Max concurrent Fingerprint Workers |
| `COVER_ART_CONCURRENCY` | usize | `4` | Max concurrent cover art fetches |
| `ENRICHMENT_CONFIDENCE_THRESHOLD` | f32 | `0.85` | AcoustID min score for `done` |
| `UNMATCHED_RETRY_LIMIT` | u32 | `3` | Retries before `unmatched` → `exhausted` |
| `FAILED_RETRY_LIMIT` | u32 | `5` | Retries before `failed` → `exhausted` |
| `DB_POOL_SIZE` | u32 | `10` | sqlx pool max connections |

---

## 13. Startup Sequence

    Parse Config from environment variables

    Connect sqlx::PgPool (DATABASE_URL, DB_POOL_SIZE)

    Run SQLx migrations (sqlx::migrate!())

    Stale enriching watchdog:
    UPDATE tracks SET enrichment_status = 'pending'
    WHERE enrichment_status = 'enriching'
    AND enriched_at < now() - INTERVAL '10 minutes'

    Construct Arc<Semaphore> SMB_READ_SEMAPHORE (SMB_READ_CONCURRENCY permits)

    Construct CancellationToken (shared with all pipeline stages)

    tokio::spawn pipeline tasks:
    a. adapters-watcher: MediaWatcher::start()
    b. Classifier task
    c. Enrichment Orchestrator task
    d. AcoustID Worker task
    e. MusicBrainz Worker task

    Connect Serenity Discord client

    Register slash commands (global, async)

    Serenity event loop (blocks until SIGTERM / CTRL+C)

    Signal received: token.cancel() → await pipeline drain → exit

text

---

## 14. Graceful Shutdown

All pipeline stages receive the same `CancellationToken`. Each stage:

```rust
loop {
    tokio::select! {
        biased;
        _ = token.cancelled() => break,
        msg = rx.recv() => match msg {
            Some(m) => process(m).await,
            None    => break,
        }
    }
}
```

Shutdown order (each waits for previous):
1. PollWatcher dropped → FileEvent channel closes
2. Classifier drains → ToFingerprint channel closes
3. Fingerprint Workers finish current `spawn_blocking` tasks → channel closes
4. Enrichment Orchestrator resets in-flight batch to `pending` → exits
5. AcoustID Worker finishes current request → exits
6. MusicBrainz Worker finishes current request → exits
7. Cover Art Fetcher + Tag Writers finish current file operation → exit
8. Serenity disconnects from Discord gateway
9. Process exits

---

## 15. External API Contracts

### AcoustID

POST https://api.acoustid.org/v2/lookup
Content-Type: application/x-www-form-urlencoded
Body: client={ACOUSTID_API_KEY}&fingerprint={chromaprint}&duration={secs}
&meta=recordings+releasegroups+compress
Rate limit: 1 req/sec (governor GCRA)

### MusicBrainz

GET https://musicbrainz.org/ws/2/recording/{mbid}
?inc=releases+release-groups+artists+genres
Accept: application/json
User-Agent: {MB_USER_AGENT} ← required; omitting causes IP ban
Rate limit: 1 req/sec (governor GCRA, separate instance)


### Cover Art Archive

GET https://coverartarchive.org/release/{release_mbid}/front-500
No authentication. Concurrency limited by COVER_ART_CONCURRENCY semaphore.


---

## 16. Invariants — Agents Must Not Violate

1. **Never use `RecommendedWatcher`, `INotifyWatcher`, or `FsEventWatcher`.**
   Always use `PollWatcher` via `new_debouncer_opt::<_, PollWatcher>`.

2. **Never use `unaccent()` directly in a generated column or functional index.**
   Always use `immutable_unaccent()`.

3. **Never expose any `enrichment_status` other than `done` to end users**
   via any Discord command, search result, or autocomplete entry.

4. **Never read file bytes from MEDIA_ROOT without holding a permit from
   `SMB_READ_SEMAPHORE`.** stat() and metadata() calls do not need a permit.

5. **Never write to a file in-place.** Always write to a tempfile in the same
   directory, then `std::fs::rename`.

6. **Never compute BLAKE3 on any file.** BLAKE3 is not used in v2.
   Change detection uses `file_modified_at` + `file_size_bytes` only.

7. **Never use `audio_fingerprint` as anything other than the deduplication
   key.** Never use file path or file size as an identity key.

8. **Never import adapter types into `domain/` or `application/`.** Dependency
   direction: adapters → application → domain only.

9. **Never store absolute paths in `blob_location`.** Always relative to
   `MEDIA_ROOT`.

10. **Never store cover art bytes in Postgres.** Paths only in DB; bytes on
    disk as `cover.jpg`.

11. **Never start a new poll scan while `scan_in_progress == true`.** Check
    the `AtomicBool` with `Ordering::Acquire` before beginning any directory walk.

12. **Never use `songbird::tracks::TrackQueue`.** Use manual `call.play_input()`
    with direct `TrackEvent` wiring.

13. **On any Songbird voice gateway error or WebSocket disconnect, drain
    `GuildQueue.items` entirely.** Never attempt to resume a queue across a
    broken voice connection.

14. **`GuildQueue.items` stores `(Uuid, TrackHandle)` pairs.** UUID and
    TrackHandle are always inserted and removed together, never independently.

15. **Never use `tokio-rayon`.** Use `tokio::task::spawn_blocking` for all
    CPU-intensive file operations (Symphonia decode, Chromaprint computation).