# TeamTI v3 — Pass 2 Prompt
## Tag Writeback: Shape Fix & Extended Field Coverage

> Components 2 (MusicBrainz work credits), 3 (LRCLIB), and 4 (cleanup)
> are already complete. This pass addresses Component 1 only.
>
> **Prerequisite:** Pass 1 (Tantivy migration) must be applied before
> this pass. The `tracks` table has `bpm`, `isrc`, `lyrics`, `genres`
> columns and `track_artists` has populated `composer`/`lyricist` rows.
>
> Reference: attach `file_ops.rs`, `tag_writer_worker.rs`,
> `tag_writer.rs`, and `tag_reader.rs` before sending to agent.

---

## Objective

Fix a destructive round-trip bug in genre writeback and extend the
writeback pipeline to cover all enriched metadata fields that are
currently stored in PostgreSQL but never written to audio file tags.

---

## Scope

**Fix:**
- `TagData.genre: Option<String>` → `genres: Vec<String>` (shape fix)
- `tag_writer.rs` uses `set_genre()` → must use `remove_key` + `push()` loop

**Extend `TagData` and `tag_writer.rs` to cover:**
- `bpm: Option<i32>`
- `isrc: Option<String>`
- `composers: Vec<String>`
- `lyricists: Vec<String>`
- `lyrics: Option<String>`

**Not in scope:** lofty version changes, new crates, schema changes,
any pipeline stage other than `TagData`, `tag_writer_worker.rs`,
`tag_writer.rs`, and `tag_reader.rs` cleanup.

---

## The Core Bug (F1)

`tag_writer_worker.rs` currently does something equivalent to:
```rust
TagData {
    genre: track.genres.map(|g| g.join(";")),
    ...
}
```

`tag_writer.rs` calls `tag.set_genre("Hip Hop;R&B")`.

On the next scan, `tag_reader.rs` reads that back as a single genre
`"Hip Hop;R&B"` — the two genres have been permanently merged into one
string. This is a destructive write.

The fix uses lofty's `push()` which appends without replacing. For
FLAC/Vorbis this produces two independent `GENRE=` comment entries.
For ID3v2, lofty uses only the first pushed item per spec (TCON is
single-valued) — still correct, just takes the first genre.

---

## Step 1 — `TagData` in `crates/application/src/ports/file_ops.rs`

Replace the `genre` field and add all new fields:

```rust
pub struct TagData {
    // existing fields — do not change these:
    pub title:        Option<String>,
    pub artist:       Option<String>,
    pub album:        Option<String>,
    pub year:         Option<i32>,
    pub track_number: Option<u32>,
    pub disc_number:  Option<u32>,

    // CHANGED: was `genre: Option<String>`
    // Vec preserves multi-value shape (e.g. ["Hip Hop", "R&B"]).
    // Empty vec means "no genre data" — writer skips the field.
    pub genres: Vec<String>,

    // NEW: all were stored in DB but never written to file tags
    pub bpm:       Option<i32>,
    pub isrc:      Option<String>,
    pub composers: Vec<String>,  // from track_artists WHERE role = 'composer'
    pub lyricists: Vec<String>,  // from track_artists WHERE role = 'lyricist'
    pub lyrics:    Option<String>,
}
```

`TagData` does not derive `Default` currently — do not add it. Use
explicit struct construction everywhere so the compiler enforces that
all callers are updated.

---

## Step 2 — `tag_writer_worker.rs`

The worker builds `TagData` from the enriched track record. Two changes:

**2a. Genre shape.**

```rust
// BEFORE (broken):
genre: track.genres.as_ref().map(|g| g.join(";")),

// AFTER:
genres: track.genres.clone().unwrap_or_default(),
// If track.genres is already Vec<String>, just:
genres: track.genres.clone(),
```

**2b. New fields from the track record.**

`bpm`, `isrc`, and `lyrics` come directly from `track.*` fields.
`composers` and `lyricists` must be fetched from `track_artists`.

Add a repository call before building `TagData`:

```rust
let composers = track_repo
    .get_artists_by_role(track.id, ArtistRole::Composer)
    .await?
    .into_iter()
    .map(|a| a.name)
    .collect::<Vec<_>>();

let lyricists = track_repo
    .get_artists_by_role(track.id, ArtistRole::Lyricist)
    .await?
    .into_iter()
    .map(|a| a.name)
    .collect::<Vec<_>>();
```

Then build `TagData`:

```rust
TagData {
    title:        ...,  // unchanged
    artist:       ...,  // unchanged
    album:        ...,  // unchanged
    year:         ...,  // unchanged
    track_number: ...,  // unchanged
    disc_number:  ...,  // unchanged
    genres:       track.genres.clone().unwrap_or_default(),
    bpm:          track.bpm,
    isrc:         track.isrc.clone(),
    composers,
    lyricists,
    lyrics:       track.lyrics.clone(),
}
```

**Repository method to add** (`TrackRepositoryPort` trait +
`TrackRepositoryImpl`):

```rust
// In trait (application/src/ports/repository.rs):
async fn get_artists_by_role(
    &self,
    track_id: Uuid,
    role:     ArtistRole,
) -> Result<Vec<ArtistRecord>, AppError>;

// ArtistRecord is whatever minimal struct holds `name: String`.
// If one already exists, reuse it. If not, define:
pub struct ArtistRecord {
    pub id:   Uuid,
    pub name: String,
}

// In TrackRepositoryImpl (adapters-persistence):
// SQL:
sqlx::query_as!(
    ArtistRecord,
    r#"
    SELECT ar.id AS "id: Uuid", ar.name
    FROM track_artists ta
    JOIN artists ar ON ar.id = ta.artist_id
    WHERE ta.track_id = $1 AND ta.role = $2
    ORDER BY ta.position ASC
    "#,
    track_id,
    role.as_str()
)
.fetch_all(&self.pool)
.await
.map_err(|e| AppError::Database { ... })
```

`ArtistRole::as_str()` must return `"composer"` and `"lyricist"` for
those variants. Confirm the existing `as_str()` implementation covers
these roles — if they were added in Component 2, they are already there.

---

## Step 3 — `tag_writer.rs`

This is the only file where lofty is called. Three changes:

**3a. Fix genre writeback (the core bug fix).**

```rust
// REMOVE entirely:
tag.set_genre(data.genre.as_deref().unwrap_or(""));
// or whatever the current single-value call looks like

// REPLACE WITH:
if !data.genres.is_empty() {
    // Remove any existing genre entries before writing.
    // Required because lofty's push() appends — without this,
    // a re-write would duplicate all genre entries.
    tag.remove_key(&ItemKey::Genre);
    for genre in &data.genres {
        tag.push(TagItem::new(
            ItemKey::Genre,
            ItemValue::Text(genre.clone()),
        ));
    }
}
```

**3b. Single-value new fields.**

Add after the existing single-value field writes (title, artist, etc.):

```rust
if let Some(bpm) = data.bpm {
    tag.insert_text(ItemKey::Bpm, bpm.to_string());
}
if let Some(ref isrc) = data.isrc {
    tag.insert_text(ItemKey::Isrc, isrc.clone());
}
if let Some(ref lyrics) = data.lyrics {
    tag.insert_text(ItemKey::Lyrics, lyrics.clone());
}
```

**3c. Multi-value composer and lyricist** (same pattern as genres):

```rust
if !data.composers.is_empty() {
    tag.remove_key(&ItemKey::Composer);
    for composer in &data.composers {
        tag.push(TagItem::new(
            ItemKey::Composer,
            ItemValue::Text(composer.clone()),
        ));
    }
}
if !data.lyricists.is_empty() {
    tag.remove_key(&ItemKey::Lyricist);
    for lyricist in &data.lyricists {
        tag.push(TagItem::new(
            ItemKey::Lyricist,
            ItemValue::Text(lyricist.clone()),
        ));
    }
}
```

**Write ordering.** Write all fields before calling `tag.save_to_path()`
or `AudioFile::save()` — do not interleave writes and saves.

**`insert_text` vs `push` — when to use which:**

| Situation | Method |
|-----------|--------|
| Single-value field (bpm, isrc, lyrics) | `insert_text(key, value)` — replaces any existing value |
| Multi-value field (genres, composers, lyricists) | `remove_key(&key)` then `push()` per value |

`insert_text` is a convenience wrapper for single-value replace.
`push` appends without deduplication — the `remove_key` before it is
mandatory to avoid doubling values on repeated writes.

---

## Step 4 — `tag_reader.rs` Cleanup

Remove the two shadowed variable declarations identified in the audit.
These are dead assignments — the values are reassigned before use:

```rust
// REMOVE these early declarations if they appear (lines ~112, ~119):
let sample_rate = ...;
let channels    = ...;
// The reassignments later in the function are the ones actually used.
```

Confirm with `cargo clippy` — `unused_assignments` or `unused_variables`
warnings will point directly at the dead lines. Remove all such warnings
in `tag_reader.rs`.

---

## Verification

```bash
# Build — zero warnings
cargo build --workspace 2>&1 | grep -E "^error|^warning"

# Query cache
cargo sqlx prepare --workspace

# Tests
cargo test --workspace

# Manual — trigger enrichment on a known multi-genre track
# (e.g. one with genres = ["Hip Hop", "R&B"] in the database)
# After TagWriter runs:
#
# For FLAC — inspect with metaflac:
metaflac --list /path/to/track.flac | grep GENRE
# Expected: two separate GENRE= entries, not one "Hip Hop;R&B"
#
# For MP3 — inspect with eyeD3 or id3v2:
eyeD3 /path/to/track.mp3 | grep -i genre
# Expected: one genre (ID3v2 TCON is single-valued — first genre wins)
#
# Verify BPM, ISRC written:
metaflac --list /path/to/track.flac | grep -E "BPM|ISRC"
#
# Verify composer written if present in track_artists:
metaflac --list /path/to/track.flac | grep COMPOSER
#
# Verify lyrics written if present:
metaflac --list /path/to/track.flac | grep -c LYRICS
```

---

## Constraints

- Do not change the lofty version.
- Do not change any pipeline stage other than the four files listed.
- If `TagData` is used anywhere outside `tag_writer_worker.rs` to
  construct the struct (e.g. tests), update those call sites too —
  the struct must compile everywhere with the new field set.
- `genres: Vec<String>` with an empty vec is the canonical "no genres"
  state. Do not use `Option<Vec<String>>`.
- Same for `composers` and `lyricists` — always `Vec<String>`, never
  `Option<Vec<_>>`.
