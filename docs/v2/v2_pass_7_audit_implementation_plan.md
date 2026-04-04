# Pass 7b: Metadata Pipeline Correctness & Completeness Audit

## Background

After the Pass 7 schema changes, a full audit reveals several correctness gaps in the metadata flow. Research confirms specific API and architectural changes needed.

## Findings (4 categories)

---

### F1. Genre Tag Shape Loss (Read ↔ Write Asymmetry)

**Read side** (correct): `tag_reader.rs` iterates all `TagItem`s with `ItemKey::Genre`, producing a `Vec<String>`. For FLAC/Vorbis this correctly captures multiple `GENRE=Hip Hop`, `GENRE=R&B` entries. For ID3v2 it captures the single TCON frame.

**Write side** (broken): `tag_writer_worker.rs` joins genres into `"Hip Hop;R&B"` and calls `set_genre()` — which calls `insert_text(ItemKey::Genre, ...)` internally, writing a single `GENRE=Hip Hop;R&B` item. On the next scan, `tag_reader` reads this back as **one genre** `"Hip Hop;R&B"` instead of two separate genres.

**Fix architecture:**
1. Replace `set_genre()` with: `remove_key(ItemKey::Genre)` then `push(TagItem::new(ItemKey::Genre, ItemValue::Text(g)))` for each genre.
2. `TagData.genre: Option<String>` → `TagData.genres: Vec<String>` to carry the shape through.
3. `tag_writer_worker.rs` passes `track.genres.clone().unwrap_or_default()` directly.

> [!IMPORTANT]
> lofty 0.23's `Tag::push()` appends without replacing — exactly what multi-value FLAC/Vorbis (`GENRE=`) tags need. For ID3v2, lofty will use only the first pushed Genre item (per spec: TCON is single-valued), so we still get correctness. The `push()` doc explicitly states: *"Multiple items of the same ItemKey are not valid in all formats, in which case the first available item will be used."*

---

### F2. Extended Tag Writeback Deficit

`TagData` and `tag_writer.rs` only write: title, artist, album, year, genre, track_number, disc_number.

The following fields are extracted on scan and stored in DB, but **never written back**:

| Field | lofty `ItemKey` | Notes |
|---|---|---|
| BPM | `ItemKey::Bpm` | String-valued |
| ISRC | `ItemKey::Isrc` | — |
| Composer | `ItemKey::Composer` | — |
| Lyricist | `ItemKey::Lyricist` | — |
| Lyrics | `ItemKey::Lyrics` | — |

**Fix:** Expand `TagData` with these optional fields. In `tag_writer.rs`, use `insert_text()` for each present field.

---

### F3. MusicBrainz API Gaps — ISRC & Composer/Lyricist

Current `inc` parameter: `releases+artists+genres+labels+release-groups`

**Missing:**
- `isrcs` — ISRCs are a first-class sub-resource on recordings; without this, `MbRecording.isrc` is always `None`.
- Composer/Lyricist live on the **Work** entity, not the Recording. Fetching them requires:
  1. Add `work-rels` to the `inc` to get the linked Work MBID.
  2. Make a second API call to `work/{id}?inc=artist-rels` to get composer/lyricist relationships from the Work's `relations` array (type = "composer" / "lyricist").

**Proposed approach:**
- Add `isrcs` to the `inc` parameter (zero-cost, single request).
- Add `work-rels` to the `inc` parameter to get linked Work IDs.
- Implement a `fetch_work_credits()` method that fetches `work/{id}?inc=artist-rels` and extracts composer/lyricist from the `relations` array.
- Call `fetch_work_credits()` from `MusicBrainzWorker` after the recording fetch, rate-limited via the existing governor bucket.
- Persist composer/lyricist as `TrackArtist` entries with `ArtistRole::Composer` / `ArtistRole::Lyricist`.

> [!WARNING]
> This adds a second API call per enrichment. The existing 1 req/sec rate limiter already handles this — the pipeline will slow down to ~0.5 tracks/sec enrichment rate. This is acceptable given MusicBrainz's rate policy.

---

### F4. Genre Overwrite Semantics

Current SQL: `genres = COALESCE($5, genres)` — if MB returns genres, they completely replace local file-tag genres.

Per user direction: **MB always wins if genres exist.** This is already the correct behavior. No change needed.

---

## Proposed Changes

### Component 1: Tag Writer Pipeline

#### [MODIFY] [file_ops.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/file_ops.rs)
- Replace `genre: Option<String>` with `genres: Vec<String>`
- Add fields: `bpm: Option<i32>`, `isrc: Option<String>`, `composer: Option<String>`, `lyricist: Option<String>`, `lyrics: Option<String>`

#### [MODIFY] [tag_writer_worker.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/tag_writer_worker.rs)
- Build `TagData` using `track.genres.clone().unwrap_or_default()` instead of `join(";")`
- Populate new fields from `track` record

#### [MODIFY] [tag_writer.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-media-store/src/tag_writer.rs)
- Replace `set_genre()` with `remove_key(ItemKey::Genre)` + `push()` loop for multi-value genres
- Add `insert_text()` calls for BPM, ISRC, Composer, Lyricist, Lyrics

---

### Component 2: MusicBrainz Enrichment

#### [MODIFY] [response.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-musicbrainz/src/response.rs)
- Add `isrcs: Vec<String>` to `MbRecordingResponse`
- Add `relations: Vec<MbRelation>` for work-rels parsing
- Add `MbWorkResponse` struct for the work lookup
- Add `MbRelation` struct with `type_`, `target`, etc.

#### [MODIFY] [lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-musicbrainz/src/lib.rs)
- Update `inc` to `releases+artists+genres+labels+release-groups+isrcs+work-rels`
- Extract first ISRC from `body.isrcs`
- Extract linked Work MBID from `body.relations` where `type_ == "performance"`
- Add `fetch_work_credits()` method returning composer/lyricist names+MBIDs

#### [MODIFY] [enrichment.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/enrichment.rs)
- Add `isrc: Option<String>` to `MbRecording`
- Add `work_mbid: Option<String>` to `MbRecording`
- Add `fetch_work_credits()` to `MusicBrainzPort` trait
- Define `MbWorkCredits` struct with `composers` and `lyricists` fields

#### [MODIFY] [musicbrainz_worker.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/musicbrainz_worker.rs)
- After recording fetch, call `fetch_work_credits()` if `work_mbid` is present
- Upsert composer/lyricist artists with `ArtistRole::Composer` / `ArtistRole::Lyricist`
- Pass `isrc` through to `update_enriched_metadata`

---

### Component 3: Persistence Layer

#### [MODIFY] [repository.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/application/src/ports/repository.rs)
- Add `isrc: Option<&str>` parameter to `update_enriched_metadata`

#### [MODIFY] [track_repository.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-persistence/src/repositories/track_repository.rs)
- Add `isrc = COALESCE($10, isrc)` to the `update_enriched_metadata` SQL
- Follow the same COALESCE pattern for consistency

---

### Component 4: Cleanup

#### [MODIFY] [tag_reader.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-media-store/src/tag_reader.rs)
- Remove shadowed `sample_rate` and `channels` variables (lines 112, 119) — they are now dead code since the reassignment at lines 136-137 is used instead

#### [MODIFY] [lib.rs](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-musicbrainz/src/lib.rs)
- Apply clippy `collapsible_if` suggestions (3 instances)

---

## Verification Plan

### Automated
```bash
cargo clippy --workspace --all-targets  # Zero warnings
cargo test --workspace                   # All tests pass
cargo sqlx prepare --workspace           # Query cache updated
```

### Manual
1. Run bot, trigger `/rescan`, verify enrichment completes
2. Query DB: verify `isrc`, `genres` (array shape) are populated
3. Inspect a FLAC file's tags after writeback — verify multiple `GENRE=` tags exist (not semicolon-joined)
4. Verify composer/lyricist appear in `track_artists` with correct roles

## Open Questions

> [!IMPORTANT]
> **Work lookup cost:** Each MB enrichment will now make 2 API calls (recording + work). This halves throughput but ensures completeness. Should we make work lookup optional via config flag, or always-on?
