# TeamTI v3 — Audit Pass 2.1
## Tag Writeback: Correctness & Completeness Review

> **Scope.** Review and correct the output of Pass 2 before integration
> testing. The four files in scope are `file_ops.rs`, `tag_writer_worker.rs`,
> `tag_writer.rs`, and `tag_reader.rs`.
>
> Fix all Critical and Major findings in order.
> Apply Optimizations unless they conflict with clarity.
>
> **Reference:** Attach Pass 2 output files before sending.

---

## Findings Index

| ID | Severity | Location | Title |
|----|----------|----------|-------|
| C1 | Critical | `tag_writer.rs` | BPM ≤ 0 written to file tags as "0" or negative string |
| C2 | Critical | `tag_writer.rs` | LRC-formatted lyrics written to plain-text lyrics tag on ID3v2 |
| C3 | Critical | `tag_writer.rs` | `remove_key` API — confirm correct lofty 0.23 method name |
| M1 | Major    | `tag_writer_worker.rs` | Two DB round-trips per track for composer + lyricist |
| M2 | Major    | `tag_writer_worker.rs` | `ArtistRecord` may duplicate an existing domain type |
| M3 | Major    | `tag_writer.rs` | `insert_text` on re-writeback for existing single-value fields |
| O1 | Optim.   | `tag_writer_worker.rs` | `track.genres.clone().unwrap_or_default()` clones unnecessarily |
| O2 | Optim.   | `tag_writer.rs` | Write ordering — all mutations before single save call |
| N1 | Note     | `tag_writer.rs` | ISRC not validated before write |
| N2 | Note     | `tag_reader.rs` | Clippy cleanup instruction is underspecified |

---

## Critical Fixes

### C1 — BPM ≤ 0 written to file tags

**File:** `tag_writer.rs`

**Problem.** `TagData.bpm` is `Option<i32>` sourced from
`tracks.bpm INTEGER` in PostgreSQL. The DB column allows any integer
value, including `0`. If a scan tool writes `bpm = 0` meaning "unknown",
Pass 2's writer emits `tag.insert_text(ItemKey::Bpm, "0")` — a literal
zero BPM tag in the audio file. Some players display this as "0 BPM",
others reject it outright, and a subsequent scan would re-read `0` and
store it as a known BPM value, silently poisoning the library.

Negative BPM values are similarly nonsensical and could appear if a
scanning tool produces erroneous data.

**Fix.** Gate the BPM write on a positive value:

```rust
// REPLACE:
if let Some(bpm) = data.bpm {
    tag.insert_text(ItemKey::Bpm, bpm.to_string());
}

// WITH:
if let Some(bpm) = data.bpm.filter(|&b| b > 0) {
    tag.insert_text(ItemKey::Bpm, bpm.to_string());
}
```

Apply the same guard symmetrically in `tag_reader.rs`: when reading
`ItemKey::Bpm`, parse to `i32` and discard values ≤ 0 before storing
to the DB. If this guard is already present in the existing reader,
leave it. If it is not, add it:

```rust
// In tag_reader.rs, when reading BPM:
let bpm = tag
    .get_string(&ItemKey::Bpm)
    .and_then(|s| s.parse::<i32>().ok())
    .filter(|&b| b > 0);  // discard 0 and negative
```

---

### C2 — LRC-formatted lyrics written to wrong tag on ID3v2

**File:** `tag_writer.rs`

**Problem.** Component 3 (LRCLIB) stores lyrics in `tracks.lyrics` as
either LRC-formatted text (with timestamps: `[00:15.12] Hello`) or plain
text, preferring synced over plain. Pass 2 writes the stored value to
`ItemKey::Lyrics` unconditionally.

The mapping of `ItemKey::Lyrics` in lofty:
- **FLAC/Vorbis:** writes to `LYRICS=` comment — players accept both
  plain and LRC-formatted text here without confusion.
- **ID3v2 (MP3):** maps to `USLT` (Unsynchronized Lyrics Text). Writing
  LRC timestamp markers (`[00:15.12]`) into a `USLT` frame causes those
  markers to appear literally in the lyrics display of most players. The
  correct ID3v2 frame for synced lyrics is `SYLT`, which lofty exposes
  via a different API — not `insert_text`.

**Fix.** Detect whether the stored lyrics string is LRC-formatted, and
for ID3v2 tags strip the timestamps before writing to `USLT`:

```rust
/// Returns true if the string contains LRC timestamp markers.
/// A timestamp line looks like: [mm:ss.xx] or [mm:ss.xxx]
fn is_lrc_format(s: &str) -> bool {
    s.lines().any(|line| {
        let t = line.trim();
        t.starts_with('[') && {
            // Check for [digits:digits pattern
            let inner = t.trim_start_matches('[');
            inner.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        }
    })
}

/// Strip LRC timestamp prefixes, returning plain text lyrics.
/// "[00:15.12] Hello\n[00:18.50] World" → "Hello\nWorld"
fn lrc_to_plain(s: &str) -> String {
    s.lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() { return None; }
            // Strip leading [timestamp] — find the closing ']'
            if t.starts_with('[') {
                t.find(']').map(|i| t[i + 1..].trim().to_owned())
                    .filter(|s| !s.is_empty())
            } else {
                Some(t.to_owned())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

Then in the lyrics write block:

```rust
if let Some(ref lyrics) = data.lyrics {
    // Detect the tag format from context.
    // In lofty, you can check `tag.tag_type()` to branch.
    use lofty::TagType;
    let write_value = match tag.tag_type() {
        TagType::Id3v2 if is_lrc_format(lyrics) => {
            // Strip timestamps — USLT does not support them.
            // Full synced-lyrics support via SYLT is deferred.
            lrc_to_plain(lyrics)
        }
        _ => lyrics.clone(),
    };
    tag.insert_text(ItemKey::Lyrics, write_value);
}
```

Note: this trades away timestamp data for MP3 files. Full SYLT support
is deferred — document with a TODO:

```rust
// TODO: SYLT (synchronized lyrics) support for ID3v2.
// Currently strips LRC timestamps and writes plain USLT.
// Implementing SYLT requires lofty's SynchronizedText API,
// not insert_text. Defer until Discord lyrics display is implemented.
```

---

### C3 — Confirm correct lofty 0.23 `remove_key` method name

**File:** `tag_writer.rs`

**Problem.** Pass 2 uses `tag.remove_key(&ItemKey::Genre)`. In lofty 0.23,
the method that removes ALL items matching a key is `remove_key`. However,
lofty's API has undergone naming changes across minor versions, and the
method may be named `retain` (with a predicate), `remove` (with a key),
or `remove_key` depending on the version.

**Fix.** Before writing the genre/composer/lyricist write blocks, check the
lofty 0.23 docs or the `Tag` trait source for the correct method:

```bash
# In the workspace:
cargo doc --package lofty --open
# Navigate to Tag → methods → search "remove"
```

Expected signatures in lofty 0.23:
```rust
// Removes the first item matching the key:
fn remove_key(&mut self, key: &ItemKey);    // or:
fn remove(&mut self, key: &ItemKey) -> bool;
```

If the method is named `remove` not `remove_key`, replace all usages.
If it only removes the FIRST match (not all), then wrap in a loop:

```rust
// If remove() only removes the first match, clear all:
while tag.remove_key(&ItemKey::Genre) {}
// or, if it returns bool:
while tag.remove(&ItemKey::Genre) {}
```

The multi-value write contract requires ALL existing values to be removed
before the push loop. If any one old value survives, re-writeback produces
duplicates. Verify this works correctly with a FLAC file that already has
two `GENRE=` entries before calling the write path.

---

## Major Fixes

### M1 — Two DB round-trips per track for composers and lyricists

**File:** `tag_writer_worker.rs`

**Problem.** Pass 2 calls `get_artists_by_role(track.id, ArtistRole::Composer)`
and then `get_artists_by_role(track.id, ArtistRole::Lyricist)` — two
separate queries per track. For a batch writeback (e.g. 10,000 tracks
re-enriched after a rescan), this is 20,000 extra queries against
`track_artists`. Each is a fast indexed lookup, but the cumulative
latency of 20k round-trips over a connection pool adds seconds of
unnecessary wait.

**Fix.** Replace with a single query that fetches both roles and
partitions in Rust:

```rust
// New repository method signature (trait + impl):
async fn get_credits(
    &self,
    track_id: Uuid,
) -> Result<TrackCredits, AppError>;

pub struct TrackCredits {
    pub composers: Vec<String>,   // names, ordered by position
    pub lyricists: Vec<String>,
}

// SQL:
sqlx::query!(
    r#"
    SELECT ar.name, ta.role, ta.position
    FROM track_artists ta
    JOIN artists ar ON ar.id = ta.artist_id
    WHERE ta.track_id = $1
      AND ta.role IN ('composer', 'lyricist')
    ORDER BY ta.role, ta.position ASC
    "#,
    track_id
)
```

Partition the result in Rust:

```rust
let mut composers = Vec::new();
let mut lyricists = Vec::new();
for row in rows {
    match row.role.as_str() {
        "composer" => composers.push(row.name),
        "lyricist" => lyricists.push(row.name),
        _ => {}
    }
}
Ok(TrackCredits { composers, lyricists })
```

Remove `get_artists_by_role` from the trait entirely if it was added
only for this use case. If it is used elsewhere, keep it but do not
call it twice from `tag_writer_worker.rs`.

---

### M2 — `ArtistRecord` may duplicate an existing domain type

**File:** `crates/application/src/ports/repository.rs` or `crates/domain/`

**Problem.** Pass 2 proposes defining `pub struct ArtistRecord { id: Uuid, name: String }`.
Before creating this type, check whether an equivalent already exists
in the domain layer — for example `Artist`, `ArtistSummary`,
`ArtistInfo`, or similar. Introducing a duplicate struct for the same
concept creates divergence and confusion about which type to use.

**Fix.** Search the workspace:

```bash
grep -rn "struct Artist" crates/domain/src/
grep -rn "struct Artist" crates/application/src/
```

If an equivalent type with at least `id: Uuid` and `name: String` fields
exists, use it. If the M1 fix above is applied (using `get_credits`
returning `TrackCredits`), then `ArtistRecord` is never needed at all —
the query returns only `name: String` and `role: String`, which map
directly into the partition logic. Remove `ArtistRecord` entirely in
that case.

---

### M3 — `insert_text` behavior on re-writeback for existing fields

**File:** `tag_writer.rs`

**Problem.** Pass 2 uses `tag.insert_text(ItemKey::Bpm, ...)` to write
single-value fields. `insert_text` replaces any existing value for that
key — this is correct on first write. However, on a re-writeback (a
track that was already written and is being re-written after new
enrichment), the existing tag value is silently replaced. This is the
correct behavior for single-value fields.

The concern is the inverse case: what if a field that now has `None`
(e.g., `isrc` was populated but then cleared from the DB) should remove
the tag from the file? Pass 2 only writes when `Some` — it never removes
a tag that was previously written but whose DB value is now `None`.

**Fix.** For each single-value field, explicitly remove the tag when the
value is `None`, so the file stays in sync with the database:

```rust
// REPLACE the simple guard:
if let Some(bpm) = data.bpm.filter(|&b| b > 0) {
    tag.insert_text(ItemKey::Bpm, bpm.to_string());
}

// WITH explicit remove-or-write:
match data.bpm.filter(|&b| b > 0) {
    Some(bpm) => { tag.insert_text(ItemKey::Bpm, bpm.to_string()); }
    None      => { tag.remove_key(&ItemKey::Bpm); }
}
```

Apply the same pattern to `isrc` and `lyrics`. For `genres`,
`composers`, and `lyricists`, the multi-value block already handles
this: if the Vec is empty, no push loop runs. But the `if !vec.is_empty()`
guard means an empty vec does NOT remove existing tags. Fix this too:

```rust
// REPLACE:
if !data.genres.is_empty() {
    tag.remove_key(&ItemKey::Genre);
    for g in &data.genres { tag.push(...); }
}

// WITH:
tag.remove_key(&ItemKey::Genre);    // always clear first
for g in &data.genres {             // push nothing if empty → tags removed
    tag.push(TagItem::new(ItemKey::Genre, ItemValue::Text(g.clone())));
}
```

Apply the same pattern to `composers` and `lyricists`.

---

## Optimizations

### O1 — Unnecessary clone of genres Vec

**File:** `tag_writer_worker.rs`

```rust
// If track.genres is Vec<String> (not Option<Vec<String>>):
// REPLACE:
genres: track.genres.clone(),

// Move instead of clone if track is not used after TagData construction:
genres: track.genres,   // moves the Vec — zero allocation
```

If `track` is used after `TagData` is built (e.g., passed to a logging
call or another function), keep the clone and add a comment explaining
why. If it is not, the move is free.

---

### O2 — Ensure all tag mutations precede a single save call

**File:** `tag_writer.rs`

**Problem.** If the existing `tag_writer.rs` structure interleaves
individual `save_to_path()` calls between field writes (unlikely but
possible from the prior single-field design), each save opens, modifies,
and closes the file independently — N file I/O operations instead of 1.

**Fix.** Confirm the write path follows this structure exactly:

```rust
// 1. Open file and read existing tags — once
let mut tagged = lofty::read_from_path(&path)?;
let tag = tagged.primary_tag_mut()
    .ok_or_else(|| /* no tag found error */)?;

// 2. All mutations — no I/O here
tag.remove_key(&ItemKey::Genre);
for g in &data.genres { tag.push(...); }
// ... all other fields ...

// 3. Save — once
tagged.save_to_path(&path, WriteOptions::default())?;
```

If any field is currently saved separately, consolidate it into this
single-save pattern.

---

## Notes (no code change required)

### N1 — ISRC not validated before write

**File:** `tag_writer.rs`

ISRC is a structured 12-character code (`CC-XXX-YY-NNNNN`). The DB
stores whatever MusicBrainz or the LRCLIB pipeline returns. Writing a
malformed ISRC to a file tag pollutes downstream tools (Beets, Picard,
streaming ingestion pipelines) that parse ISRC for deduplication.

For Pass 2.1, add a length guard as a minimal safeguard:

```rust
if let Some(ref isrc) = data.isrc {
    // ISRC is exactly 12 characters (no hyphens in canonical form)
    // or 15 with hyphens. Reject anything clearly malformed.
    let canonical = isrc.replace('-', "");
    if canonical.len() == 12 {
        tag.insert_text(ItemKey::Isrc, isrc.clone());
    } else {
        tracing::warn!(
            isrc = %isrc,
            operation = "tag_writer.isrc_invalid",
            "skipping malformed ISRC — expected 12 chars (without hyphens)"
        );
        tag.remove_key(&ItemKey::Isrc);
    }
}
```

Full regex validation (`[A-Z]{2}[A-Z0-9]{3}[0-9]{7}`) can be added
in a later pass if it becomes necessary.

### N2 — Step 4 cleanup instruction is underspecified

**File:** `tag_reader.rs`

The Pass 2 instruction "remove shadowed variable declarations at lines
~112, ~119" may not match the actual line numbers in the agent's
working copy. Do not guess line numbers — run clippy and fix whatever
it reports in `tag_reader.rs`:

```bash
cargo clippy -p adapters-media-store -- -D warnings
```

Fix every warning in `tag_reader.rs` reported by this command.
No other changes to `tag_reader.rs` are in scope.

---

## Verification

```bash
# 1. Zero warnings — must be clean before proceeding
cargo clippy --workspace --all-targets -- -D warnings

# 2. Query cache
cargo sqlx prepare --workspace

# 3. Unit tests
cargo test --workspace

# 4. Integration — FLAC multi-genre round-trip
# Before: a track with genres = ["Hip Hop", "R&B"] in the DB
# Trigger TagWriter on that track
# After:
metaflac --list /path/to/track.flac | grep GENRE
# Expected output (two separate lines):
# comment[N]: GENRE=Hip Hop
# comment[N]: GENRE=R&B
# NOT: comment[N]: GENRE=Hip Hop;R&B

# 5. Integration — LRC lyrics on MP3
# A track with LRC-formatted lyrics in tracks.lyrics
# After TagWriter runs:
eyeD3 /path/to/track.mp3 | grep -A5 "Lyrics"
# Expected: plain text without [mm:ss.xx] markers in the USLT frame

# 6. Integration — BPM=0 guard
# A track with bpm = 0 in the DB
# After TagWriter runs:
metaflac --list /path/to/track.flac | grep BPM
# Expected: no BPM tag (not "BPM=0")

# 7. Integration — re-writeback clears removed fields
# Set isrc = NULL in DB for a previously-written track
# Trigger TagWriter
# After:
metaflac --list /path/to/track.flac | grep ISRC
# Expected: no ISRC tag
```
