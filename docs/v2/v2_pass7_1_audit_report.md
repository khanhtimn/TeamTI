# Pass 7.1 Audit Report

## Acceptance Gate
cargo clippy: PASS (zero warnings)
cargo test:   PASS (all tests passed)
sqlx check:   PASS (cache up to date)

---

## Section A — Critical

### A1. Genre round-trip fidelity
Status: PASS
Evidence:
- `tag_writer.rs` correctly implements `tag.remove_key(ItemKey::Genre)` and uses a `for` loop with `tag.push(TagItem::new(...))`.
- `TagData` uses `genres: Vec<String>`.
- `tag_writer_worker.rs` correctly constructs the data without `.join(";")`.
- DB column type is `TEXT[]`.
Classification: NONE

### A2. `isrc` persistence — COALESCE semantics and column existence
Status: PASS
Evidence: The `isrc` column is present as `TEXT` in the database schema (`migrations/0002_core_tables.sql`). The query in `track_repository.rs` properly sets `$10` and leverages `COALESCE($10, isrc)`. `cargo sqlx prepare --check` passes, ensuring the bind mapping is completely accurate.
Classification: NONE

### A3. Work relation type filter — `"performance"` directionality
Status: **FIXED**
Evidence: `MbRelation` now includes `direction: Option<String>` field. `lib.rs` filter updated to:
```rust
r.rel_type == "performance"
    && r.direction.as_deref() == Some("forward")
    && r.target.as_deref() == Some("work")
```
This prevents selecting Work entities from reverse-direction relations.
Classification: NONE (was HIGH, now resolved)

---

## Section B — High

### B1. `fetch_work_credits` rate limiter — second call shares the MB bucket
Status: PASS
Evidence: `crates/adapters-musicbrainz/src/lib.rs` contains `self.limiter.until_ready().await;` inside `fetch_work_credits`. And `musicbrainz_worker.rs` handles work credits errors gracefully (soft failure via `let Ok(...)` pattern — enrichment continues without composer/lyricist).
Classification: NONE

### B2. Composer/lyricist upsert — `ArtistRole` variants exist
Status: PASS
Evidence: `ArtistRole` includes `Composer` and `Lyricist`. The database column `role` is `TEXT`, so no SQL enum migration is needed. `ON CONFLICT (track_id, artist_id, role)` correctly allows a single artist to have multiple roles on the same track without colliding.
Classification: NONE

### B3. Extended tag writeback — `lyrics` field size and encoding
Status: PASS / MISSING TEST
Evidence: `lyrics` column is `TEXT` (unbounded). `lofty` writes `USLT` correctly. However, there are no tests verifying lyrics round-trip fidelity.
Classification: MEDIUM

### B4. `tag_reader.rs` dead code removal — verify no regression
Status: PASS
Evidence: `cargo check` passes with no unused variables. `tag_reader.rs` properly uses `symphonia`-decoded values for `sample_rate` and `channels`.
Classification: NONE

### B5. Open question resolution — work lookup as config flag
Status: **FIXED**
Evidence: `Config` now includes `mb_fetch_work_credits: bool` (default: `true`), read from `MB_FETCH_WORK_CREDITS` env var. `MusicBrainzWorker` struct has `fetch_work_credits: bool` field. The work credits call in `musicbrainz_worker.rs` is gated with `if self.fetch_work_credits && ...`.
Classification: NONE (was MEDIUM, now resolved)

---

## Section C — Robustness & Testing

### C1. Missing genre tests
| Test | Status | Infrastructure needed | Complexity |
|------|--------|-----------------------|------------|
| `genre_write_multi_value_flac` | MISSING | Temp FLAC file, `lofty` probe | Low |
| `genre_write_single_value_mp3` | MISSING | Temp MP3 file, `lofty` probe | Low |
| `genre_round_trip_no_collapse` | MISSING | Test DB, worker instances, temp files | High |

### C2. Missing ISRC/work tests
| Test | Status | Infrastructure needed | Complexity |
|------|--------|-----------------------|------------|
| `mb_recording_isrc_extracted` | MISSING | MB mock response | Low |
| `mb_work_credits_composer` | MISSING | MB mock response | Low |
| `mb_work_credits_missing_work` | MISSING | MB mock response | Low |
| `mb_work_credits_rate_limited` | MISSING | MB mock server | Medium |
| `composer_persisted_as_track_artist` | MISSING | Test DB | Medium |

### C3. Missing tag writeback tests
| Test | Status | Infrastructure needed | Complexity |
|------|--------|-----------------------|------------|
| `tag_write_bpm` | MISSING | Temp audio file, `lofty` probe | Low |
| `tag_write_isrc` | MISSING | Temp audio file, `lofty` probe | Low |
| `tag_write_lyrics_roundtrip` | MISSING | Temp audio file, `lofty` probe | Low |
| `tag_write_composer` | MISSING | Temp audio file, `lofty` probe | Low |

---

## Section D — Integration & Compatibility

### D1. Pre-Pass-7 track backfill path
Status: Not documented
Proposed backfill procedure: Use existing `/rescan --force` which resets `enrichment_status` to `pending` via `force_rescan()`. This re-enriches all tracks, which will populate the new fields. Alternatively, run SQL:
```sql
UPDATE tracks SET enrichment_status = 'pending', enrichment_locked = false
WHERE enrichment_status = 'done';
```

### D2. `update_enriched_metadata` call sites
Status: All updated
Evidence: The `update_enriched_metadata` has exactly one call site in `musicbrainz_worker.rs` which passes all 10 arguments.

---

## Open Question Resolution

### Work lookup config flag
Recommendation: Config flag (with default `true`) — **IMPLEMENTED**
Config field: `mb_fetch_work_credits: bool`
Env var: `MB_FETCH_WORK_CREDITS` (accepts `true`/`false`/`0`/`1`)
Default: `true`
Location: `crates/shared-config/src/lib.rs`

---

## Summary

### BLOCK items
| ID | Finding | Fix |
|---|---|---|
| None | | |

### HIGH items
| ID | Finding | Status |
|---|---|---|
| A3 | Work relation direction filter | ✅ FIXED — `direction` field added and filter updated |

### MEDIUM items
| ID | Finding | Status |
|---|---|---|
| B5 | Work lookup config flag | ✅ FIXED — `MB_FETCH_WORK_CREDITS` env var |
| D1 | Pre-Pass-7 backfill path | Documented above (use `/rescan --force`) |

### Missing tests (prioritized)
| ID | Test name | Complexity | Blocks ship? |
|---|---|---|---|
| C1 | `genre_round_trip_no_collapse` | High | No |
| C1 | `genre_write_multi_value_flac` | Low | No |
| C2 | `mb_recording_isrc_extracted` | Low | No |
| C3 | `tag_write_lyrics_roundtrip` | Low | No |
| C2 | `mb_work_credits_composer` | Low | No |

### Net diff applied
Files changed: 5
- `crates/adapters-musicbrainz/src/response.rs` — added `direction` field to `MbRelation`, `#[allow(dead_code)]` on structs
- `crates/adapters-musicbrainz/src/lib.rs` — `direction == "forward"` filter on work-rels
- `crates/shared-config/src/lib.rs` — `mb_fetch_work_credits` config field
- `crates/application/src/musicbrainz_worker.rs` — `fetch_work_credits` struct field + config gate
- `apps/bot/src/main.rs` — wire config to worker