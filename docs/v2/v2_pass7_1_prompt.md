# TeamTI v2 — Pass 7.1 Prompt
## Audit: Metadata Pipeline Correctness & Completeness (Post-Pass 7)

> Review-only pass. No changes are applied without explicit approval.
> Read the full Pass 7 implementation before starting the checklist.
> The agent must verify each finding category independently — do not
> assume the implementation plan was executed correctly or completely.

---

### Context

Pass 7 widened the enrichment pipeline in four areas:

1. **F1 — Genre shape fix:** `tag_writer.rs` was changed from `set_genre()`
   to a `remove_key + push()` loop for multi-value genre support. `TagData`
   changed from `genre: Option<String>` to `genres: Vec<String>`.

2. **F2 — Extended tag writeback:** `TagData` now includes `bpm`, `isrc`,
   `composer`, `lyricist`, `lyrics`. `tag_writer.rs` writes each via
   `insert_text()`.

3. **F3 — MusicBrainz work credits:** The recording fetch now includes
   `isrcs+work-rels` in the `inc` parameter. A new `fetch_work_credits()`
   method fetches `work/{id}?inc=artist-rels` for composer/lyricist.
   These are persisted as `TrackArtist` rows with `ArtistRole::Composer`
   and `ArtistRole::Lyricist`.

4. **F4 — Genre overwrite semantics:** Confirmed as correct (MB wins).
   No code change was required.

**Open question from Pass 7 not yet resolved:**
> Should work lookup (`fetch_work_credits`) be optional via a config flag,
> or always-on? This pass must surface a recommendation.

---

### Acceptance Gate

Before any finding is investigated, run:

```bash
cargo clippy --workspace --all-targets 2>&1
cargo test --workspace 2>&1
cargo sqlx prepare --check --workspace 2>&1
```

If any command fails, that is itself a CRITICAL finding. List the full
output and classify before proceeding to the checklist.

---

### Audit Checklist

---

#### SECTION A — Critical: Data Correctness

**A1. Genre round-trip fidelity — read/write asymmetry fully resolved**

The core of F1 was that `set_genre()` collapses multi-value genres into
a semicolon-joined string on write, which `tag_reader.rs` reads back as
a single genre on the next scan.

Verify the fix is complete across the entire pipeline:

**a) `tag_writer.rs`**
Check that the write path uses exactly this pattern:
```rust
tag.remove_key(&ItemKey::Genre);
for g in &tags.genres {
    tag.push(TagItem::new(ItemKey::Genre, ItemValue::Text(g.clone())));
}
```
Common failure modes:
- `set_genre()` still present anywhere in the file (would negate the fix)
- `remove_key` called AFTER `push` (would clear the genres just written)
- `genres` iterated but the old `set_genre()` also called on a different
  branch (e.g. a fallback path for empty `genres` vec)

**b) `TagData` struct in `file_ops.rs`**
Verify `genre: Option<String>` is gone and `genres: Vec<String>` is present.
Check all construction sites of `TagData` in `tag_writer_worker.rs` —
verify none still pass `join(";")` or call `.genre =`.

**c) `tag_writer_worker.rs`**
Verify the `TagData` is built with:
```rust
genres: track.genres.clone().unwrap_or_default(),
```
Not with any intermediate string conversion.

**d) Persistence layer — `genres` column type**
Verify the `tracks.genres` DB column is `TEXT[]` (PostgreSQL array), not
`TEXT`. If it is `TEXT`, then `track.genres.clone()` returns `Option<String>`
not `Vec<String>` and the worker cannot build `TagData.genres: Vec<String>`
without conversion — which may be silently incorrect.

Run:
```sql
SELECT column_name, data_type
FROM information_schema.columns
WHERE table_name = 'tracks' AND column_name = 'genres';
```
Expected: `data_type = 'ARRAY'`.

**e) FLAC multi-genre write verification**
The lofty 0.23 `push()` behavior for multi-value FLAC/Vorbis is confirmed
correct. Verify for ID3v2 (MP3):
- After writing genres `["Hip Hop", "R&B"]` to an MP3, re-read with lofty
- Expected: `tag_reader` returns `vec!["Hip Hop"]` (ID3v2 TCON is single-valued,
  lofty uses first item per spec — this is CORRECT and acceptable)
- Failure: both genres are written to a single TCON frame as `Hip Hop;R&B`
  which would be read back as one item — the old bug recurring

Does any integration test verify this round-trip? If not, flag as MISSING TEST.

Severity: **Critical** — if the fix is incomplete, every rescan corrupts
multi-genre FLAC files by collapsing their genres.

---

**A2. `isrc` persistence — COALESCE semantics and column existence**

F3 adds `isrc = COALESCE($10, isrc)` to `update_enriched_metadata`.

Verify:
1. The `isrc` column exists in the `tracks` table (check migration history)
2. The SQL parameter index `$10` matches the actual parameter count in
   `update_enriched_metadata` — if any parameter was added or removed
   in Pass 7 without updating all indices, every call silently corrupts
   a different field
3. `sqlx::query!` macro validates this at compile time IF `cargo sqlx prepare`
   has been run — verify the offline cache is fresh

Count the parameters in `update_enriched_metadata` SQL manually and verify
the binding call site passes them in the same order.

Severity: **Critical** — parameter index mismatch corrupts DB rows silently;
`sqlx` offline cache prevents compile-time detection if stale.

---

**A3. Work relation type filter — `"performance"` is not always the right key**

In `lib.rs` (MusicBrainz adapter):
```rust
.filter(|r| r.type_ == "performance")
```

MusicBrainz work-rels use the following relation types to link a Recording
to its Work:
- `"performance"` — the recording is a performance of the work ✓
- `"medley"` — the recording is a medley that includes the work (multiple works)
- `"based on"` — derivative work

The implementation filters only `"performance"`. This is correct for the
common case. However, for medleys and mashups, no Work MBID will be found,
and composer/lyricist will silently be absent.

More critically: MusicBrainz returns relations in both directions. The
`relations` array on a Recording response includes relations where the
Recording is the **source** and where it is the **target**. For work-rels,
the Recording is always the source and the Work is the target. Verify the
filter also checks `r.direction == "forward"` (or equivalent) to avoid
picking up a Work that is related in the reverse direction.

Check in `response.rs`:
- Is `direction` a field on `MbRelation`?
- If yes: does the filter in `lib.rs` include `&& r.direction == "forward"`?
- If no: is the relation direction guaranteed by the API response shape
  for `work-rels` specifically?

Severity: **High** — wrong Work MBID selected for reverse-direction relations;
composer/lyricist attributed to wrong people on specific tracks.

---

#### SECTION B — High: Correctness

**B1. `fetch_work_credits` rate limiter — second call shares the MB bucket**

The implementation plan noted that `fetch_work_credits()` is rate-limited
via the existing 1 req/sec governor bucket. Verify this is actually
implemented and not just intended:

Check `adapters-musicbrainz/src/lib.rs`:
- Does `fetch_work_credits()` call `self.rate_limiter.until_ready().await`
  before the HTTP request?
- Or does it call `self.client.get(...)` directly without rate limiting?

If the rate limiter is not called, `fetch_work_credits` makes an unthrottled
second request immediately after the recording fetch. For a 10,000 track
library, this triggers MusicBrainz's IP-based rate limiting within minutes
of a full enrichment run.

Also verify: what happens when `fetch_work_credits` returns `Err`? Does
`musicbrainz_worker.rs` treat this as a non-fatal soft failure (log warning,
continue enrichment without composer/lyricist) or a hard failure (mark
track as `Failed`)? The correct behavior is **soft failure** — missing
composer/lyricist should not block a track from reaching `done`.

Severity: **High** — unthrottled HTTP causes MusicBrainz IP ban within
hours of a large enrichment run.

---

**B2. Composer/lyricist upsert — `ArtistRole` variants exist**

`musicbrainz_worker.rs` now upserts artists with:
```rust
ArtistRole::Composer
ArtistRole::Lyricist
```

Verify both variants exist in the `ArtistRole` enum in `domain/src/`.
If they were not present before Pass 7 and were added in Pass 7, verify:
1. The migration for `track_artists.role` includes these values if `role`
   is a PostgreSQL enum (not a TEXT column)
2. `ArtistRole`'s `sqlx::Type` derive or manual impl maps these correctly
3. The existing `upsert_track_artist` SQL handles the new roles — check
   `ON CONFLICT ... DO UPDATE SET role = EXCLUDED.role` covers the case
   where a track was previously enriched with `role = Primary` and is
   now being re-enriched and gaining a `Composer` entry for the same artist

Severity: **High** — compile failure if variants missing; silent DB type
error if PostgreSQL enum not updated.

---

**B3. Extended tag writeback — `lyrics` field size and encoding**

`TagData.lyrics: Option<String>` and `tag_writer.rs` writes it via
`insert_text(ItemKey::Lyrics, ...)`.

Potential issues:
1. **ID3v2 USLT vs COMM:** lofty's `ItemKey::Lyrics` maps to the `USLT`
   (Unsynchronized Lyrics) frame in ID3v2, not `COMM`. Some players
   expect `USLT`; others read only `COMM`. Verify lofty's behavior matches
   expectations for the primary audio format in use.
2. **Size:** Full lyrics can be several kilobytes. Verify the `tracks.lyrics`
   column in PostgreSQL is `TEXT` (unbounded), not `VARCHAR(n)`.
3. **Encoding:** lofty writes `USLT` as UTF-16 for ID3v2. If the DB stores
   UTF-8 and there is no encoding normalization, a re-read of the tag may
   produce different bytes than what was written. This is unlikely to cause
   data loss but verify round-trip fidelity is tested.

For this pass: flag as MISSING TEST if no test verifies lyrics writeback
and re-read.

Severity: **Medium** — lyrics not a core field; but wrong behavior here
causes user-visible corruption of file tags.

---

**B4. `tag_reader.rs` dead code removal — verify no regression**

Pass 7 removed shadowed `sample_rate` and `channels` variables at lines
112 and 119. Verify:

1. `cargo check` produces zero warnings in `tag_reader.rs` after removal
2. The `sample_rate` and `channels` values actually used (from lines 136–137)
   are the Symphonia-decoded values, not the lofty tag values. This is
   correct — Symphonia reads the actual audio stream properties, lofty
   reads the tag-declared properties (which can be wrong for poorly tagged
   files). Verify the variable at line 136–137 is the one that flows into
   the returned struct.
3. If `sample_rate` / `channels` appear anywhere else in the file, confirm
   they refer to the correct (post-136) binding.

Severity: **Medium** — dead code removal is safe, but verify the remaining
binding is the right one.

---

**B5. Open question resolution — work lookup as config flag**

The Pass 7 plan left this open:
> Should `fetch_work_credits` be optional via a config flag, or always-on?

This pass must produce a recommendation. Evaluate both options:

**Always-on:**
- Pros: complete metadata for all tracks; no config surface to explain
- Cons: halves enrichment throughput (~0.5 tracks/sec); doubles MB API calls;
  some tracks have no Work entity (live recordings, DJ sets, field recordings)
  — these always make a wasted second API call that returns 404 or no useful data

**Config flag (`MB_FETCH_WORK_CREDITS=true/false`, default: `true`):**
- Pros: operator can disable for initial bulk enrichment (fast path), then
  re-enable for incremental enrichment; reduces API pressure on large libraries
- Cons: adds a config surface; users who leave it default may not realize
  throughput implications

**Recommendation to evaluate:**
Given that v2 targets a personal/small-team NAS library (not a 50k+ track
production deployment), the throughput penalty is unlikely to matter in
practice. However, the correct architectural choice is to **make it
configurable with a `true` default**, because:
1. The throughput impact is real and documented
2. Config flags are low-cost to add
3. It follows the precedent set by `SMB_READ_CONCURRENCY`,
   `TAG_WRITE_CONCURRENCY`, etc.

The agent should evaluate this recommendation, agree or disagree with
justification, and propose the exact config field name, type, default,
and location in `shared-config`.

Severity: **Medium** — not a correctness issue; architectural decision point.

---

#### SECTION C — Robustness & Testing

**C1. Missing tests — genre round-trip**

Verify whether the following tests exist. If any are absent, flag as
MISSING TEST with suggested implementation:

| Test | What it verifies |
|------|-----------------|
| `genre_write_multi_value_flac` | Write `["Hip Hop", "R&B"]` to FLAC temp file; re-read with lofty; assert two separate `GENRE=` items |
| `genre_write_single_value_mp3` | Write `["Hip Hop", "R&B"]` to MP3 temp file; re-read; assert single TCON frame with value `"Hip Hop"` (first item only — ID3v2 spec) |
| `genre_round_trip_no_collapse` | Full pipeline: scan FLAC with two genres → enrich → tag write → re-scan → assert `tracks.genres = ARRAY['Hip Hop','R&B']` |

**C2. Missing tests — ISRC and work credits**

| Test | What it verifies |
|------|-----------------|
| `mb_recording_isrc_extracted` | Mock MB response with `isrcs: ["GBAYE0000001"]`; assert `MbRecording.isrc == Some("GBAYE0000001")` |
| `mb_work_credits_composer` | Mock work response with composer relation; assert `fetch_work_credits` returns correct composer name |
| `mb_work_credits_missing_work` | Recording with no work-rels; assert `fetch_work_credits` not called (or returns empty gracefully) |
| `mb_work_credits_rate_limited` | Verify rate limiter called before `fetch_work_credits` HTTP request |
| `composer_persisted_as_track_artist` | Integration: after MB enrichment, assert `track_artists` contains row with `role = 'composer'` |

**C3. Missing tests — extended tag writeback**

| Test | What it verifies |
|------|-----------------|
| `tag_write_bpm` | Write `bpm: Some(120)` to temp file; re-read; assert BPM tag present |
| `tag_write_isrc` | Write `isrc: Some("GBAYE0000001")`; re-read; assert ISRC tag present |
| `tag_write_lyrics_roundtrip` | Write lyrics string; re-read; assert string matches (encoding-safe) |
| `tag_write_composer` | Write `composer: Some("John Lennon")`; re-read; assert composer tag present |

For each missing test: note the infrastructure required (fixture audio file,
mock HTTP, test DB) and estimated complexity.

---

#### SECTION D — Integration & Backward Compatibility

**D1. Tracks enriched before Pass 7 — re-enrichment path**

Tracks that completed enrichment before Pass 7 have:
- `composer`, `lyricist`, `isrc` = NULL (columns may not have existed)
- `genres` = single string value if the column type was TEXT, or an array
  with semicolon-joined content

Verify:
1. The `update_enriched_metadata` SQL handles NULL gracefully for the new
   fields — COALESCE ensures this for `isrc` but confirm for `genres`
2. Pre-Pass 7 enriched tracks will NOT be re-enriched automatically unless
   their `tags_written_at` is reset or `enrichment_status` is reset to `pending`
3. If an operator wants to backfill composer/lyricist for already-enriched
   tracks, what is the mechanism? Is there a SQL script or a `/rescan` path
   that forces re-enrichment?

Document the backfill path explicitly. If none exists, flag as a missing
operational procedure.

**D2. `update_enriched_metadata` parameter count — all call sites updated**

Adding `isrc` as a new parameter to `update_enriched_metadata` changes
its signature. Verify every call site in the codebase passes the `isrc`
argument:

```bash
grep -rn "update_enriched_metadata" --include="*.rs" .
```

Every call site must have been updated. A call site that was not updated
will fail to compile — but only if `sqlx` offline cache is fresh. If the
cache is stale, the compile may succeed while silently passing wrong values.

Severity: **High** — stale sqlx cache masks parameter arity errors.

---

### Diagnostic Commands

Run all of the following. Include output in the report:

```bash
# 1. Build + clippy gate
cargo clippy --workspace --all-targets 2>&1

# 2. Tests
cargo test --workspace 2>&1

# 3. sqlx cache check
cargo sqlx prepare --check --workspace 2>&1

# 4. Genre field type in DB (requires live DB or migration inspection)
grep -rn "genres" crates/adapters-persistence/migrations/ --include="*.sql"

# 5. TagData construction sites — verify no join(";") remains
grep -rn "join.*;\|set_genre\|\.genre\s*=" \
    --include="*.rs" \
    crates/application/src/ \
    crates/adapters-media-store/src/

# 6. Rate limiter call before fetch_work_credits
grep -n "until_ready\|rate_limit\|fetch_work_credits" \
    crates/adapters-musicbrainz/src/lib.rs

# 7. ArtistRole variants
grep -rn "ArtistRole" --include="*.rs" crates/domain/src/

# 8. update_enriched_metadata call sites
grep -rn "update_enriched_metadata" --include="*.rs" .

# 9. Work relation direction filter
grep -n "direction\|performance\|forward" \
    crates/adapters-musicbrainz/src/lib.rs \
    crates/adapters-musicbrainz/src/response.rs

# 10. isrc column in migrations
grep -rn "isrc" crates/adapters-persistence/migrations/ --include="*.sql"

# 11. Lyrics column type
grep -rn "lyrics" crates/adapters-persistence/migrations/ --include="*.sql"

# 12. Test file inventory for new tests
find . -path "*/target" -prune -o -name "*.rs" -print \
    | xargs grep -l "genre\|isrc\|work_credits\|composer\|lyricist" 2>/dev/null \
    | grep -E "test|spec"
```

---

### Findings Report Format

```markdown
# Pass 7.1 Audit Report

## Acceptance Gate
cargo clippy: PASS / FAIL (output if FAIL)
cargo test:   PASS / FAIL (output if FAIL)
sqlx check:   PASS / FAIL (output if FAIL)

---

## Diagnostic Output
[per-command output]

---

## Section A — Critical

### A1. Genre round-trip fidelity
Status: PASS / PARTIAL / FAIL
Evidence: <code quote or "no issues">
Fix (if needed): ...
Classification: BLOCK / HIGH / MEDIUM

[A2, A3...]

---

## Section B — High

[B1–B5 per finding format]

---

## Section C — Robustness & Testing

### C1. Missing genre tests
| Test | Status | Infrastructure needed | Complexity |

### C2. Missing ISRC/work tests
| Test | Status | Infrastructure needed | Complexity |

### C3. Missing tag writeback tests
| Test | Status | Infrastructure needed | Complexity |

---

## Section D — Integration & Compatibility

### D1. Pre-Pass-7 track backfill path
Status: Documented / Not documented
Proposed backfill procedure: ...

### D2. update_enriched_metadata call sites
Status: All updated / N missing
[list of missing sites if any]

---

## Open Question Resolution

### Work lookup config flag
Recommendation: Always-on / Config flag (with default)
Justification: ...
Proposed config field: ...
Location in shared-config: ...

---

## Summary

### BLOCK items
| ID | Finding | Fix |

### HIGH items
| ID | Finding | Fix |

### MEDIUM items
| ID | Finding |

### Missing tests (prioritized)
| ID | Test name | Complexity | Blocks ship? |

### Net diff scope if all findings addressed
Files changed: N
Lines added: N
Lines removed: N
```

---

### Constraints

- Do not apply fixes without approval. Report and classify first.
- Exception: if `cargo clippy` or `cargo test` fails, fix the minimum
  necessary to unblock diagnostics and note exactly what was changed.
- Do not add new pipeline stages or new enrichment sources beyond what
  Pass 7 defined.
- All test proposals must be realistic — specify the exact fixture files,
  mock setup, and assertions. Vague test descriptions are not acceptable.
- The open question (work lookup config flag) must be resolved with a
  concrete recommendation, not left open.

---

### REFERENCE

docs/v2/v2_master.md
docs/v2/v2_pass7_audit_implementation_plan.md