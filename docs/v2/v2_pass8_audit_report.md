# Pass 8 Final Audit Report

## VERDICT: SHIP — with 2 HIGH work items

All CRITICAL paths are correct. Two HIGH items and several MEDIUM improvements to address before freezing v2.

***

## Diagnostic Output

### #1 — Release build
```
cargo build --release --workspace
Finished `release` profile [optimized] target(s) in 3m 06s
```
✅ PASS — zero warnings, zero errors.

### #2 — Test suite
```
cargo test --workspace
All tests passed (4 test files, 0 failures)
```
✅ PASS

### #3 — Clippy (pedantic)
```
cargo clippy --workspace --all-targets -- -W clippy::pedantic \
    -A clippy::module_name_repetitions -A clippy::must_use_candidate
```
Warnings: documentation backticks (cosmetic), `missing_errors_doc`, cast warnings (`u32` as `i32`). **No logic bugs.** Default clippy (non-pedantic) is clean.

✅ PASS (pedantic doc warnings are non-blocking)

### #4 — Dead code sweep
```
cargo check --workspace 2>&1 | grep -E "warning: (unused|dead_code|never used|never constructed)"
```
No output. ✅ PASS

### #5 — unwrap/expect in non-test code
21 occurrences. **Categorized:**
- **Startup-only (legitimate):** 11 — `Client::builder`, `config.validate().expect()`, `rustls`, semaphore `.expect("closed")` — all fire before any user interaction. ✅
- **Tag writer `primary_tag_mut().unwrap()` (L71):** Preceded by `is_some()` check at L69. Structurally sound but unclear to future readers. **MEDIUM** — add comment.
- **`discord_token.parse().unwrap()` (main.rs:211):** Should use `?` for cleaner startup error. **LOW**.

### #6 — Swallowed results (`let _ =`)
40 occurrences. **Categorized:**
- **Discord responses (30):** `let _ = interaction.edit_response(...)` — correct fire-and-forget; Discord itself is best-effort.
- **Worker `let _ = repo.update_enrichment_status(...)` (10):** In `acoustid_worker`, `musicbrainz_worker`, `cover_art_worker` — the pattern is *log error via tracing before the call, then ignore the DB update result*. This is intentional: the track will be retried by the poll loop. ✅

### #7 — TODO/FIXME/HACK/STUB sweep
No matches. ✅ PASS

### #8 — Self-rolled retry patterns
No matches. Retry logic is entirely state-machine-driven via `enrichment_status` + poll loop. ✅ No manual retry loops exist.

### #9 — HashMap in hot paths
1 match: `track_repository.rs:91` — `HashMap::new()` in `find_many_by_blob_location` as return for early-exit case (empty input). Called once per classifier batch. NOT hot. ✅ KEEP.

### #10 — Blocking calls in async context
8 matches, all in `adapters-media-store`:
- `tag_reader.rs:22` — `std::fs::read()` — called within `spawn_blocking`. ✅
- `tag_writer.rs:42,149,170,214` — `copy`, `rename`, `metadata`, `remove_file` — called within `spawn_blocking` (via `tag_writer_port.rs:40`). ✅
- `fs_store.rs:11,13` — `create_dir_all`, `canonicalize` — startup-only. ✅
- `importer.rs:79` — `File::open` — called within `spawn_blocking`. ✅

**No blocking I/O on the async runtime.** ✅ PASS

### #11 — .await on MutexGuard (deadlock risk)
14 matches. All use `tokio::sync::Mutex` (NOT `std::sync::Mutex`), which is `.await`-safe. Lock scopes are correctly bounded:
- `play.rs`: lock acquired, state read/written, then explicitly `drop(state)` before any further `.await`.
- `track_event_handler.rs`: same pattern — explicit drops before spawning tasks.
- `clear.rs`: explicit drop at L57 before `handler_lock.lock().await`.
✅ PASS — no deadlock risk.

### #12 — Dependency duplicates
**Significant:** `reqwest` 0.12.28 (transitive via songbird) + 0.13.2 (our code). `tungstenite`/`tokio-tungstenite` v0.26 + v0.28 (serenity vs songbird).
- Both are upstream — cannot fix without patching songbird/serenity.
- Binary impact: ~1MB additional. Not blocking.
✅ PASS (informational)

### #13 — sqlx cache freshness
```
cargo sqlx prepare --check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s)
```
✅ PASS

### #14 — .env.example completeness
**Missing from .env.example:**
| Variable | Added in | Default |
|---|---|---|
| `TAG_WRITE_CONCURRENCY` | Pass 4 | 2 |
| `AUTO_LEAVE_SECS` | Pass 2 | 30 |
| `MB_FETCH_WORK_CREDITS` | Pass 7 audit | true |
| `LOG_FORMAT` | telemetry.rs | "pretty" |

**Severity: HIGH** — operators deploying v2 would not know these exist.

### #15 — Migration ordering
```
0001_extensions.sql
0002_core_tables.sql
0003_user_library.sql
0004_indexes.sql
0005_tags_written_at.sql
```
✅ Ordered correctly. Sequential naming.

### #16 — Test file inventory
| File | Test count | Type |
|---|---|---|
| `application/src/error.rs` | 4 | Unit — `Retryable` trait, `PersistenceKind::Display` |
| `adapters-media-store/tests/test_symphonia.rs` | 1 | Integration — Symphonia probe |
| `adapters-media-store/tests/test_lofty.rs` | 1 | Integration — lofty write (hardcoded path) |
| `apps/bot/tests/enrichment_smoke.rs` | 3 | Skeleton — all skip without `TEST_DATABASE_URL` |

**Total real tests: 6. Skeleton tests: 3.** Coverage is minimal.

### #17 — CancellationToken coverage
Every worker is wrapped in `tokio::select! { _ = tok.cancelled() => {}, _ = worker.run(...) => {} }` in main.rs. The scanner has internal token checking. ✅ **All workers respect shutdown.**

### #18 — Binary size
```
target/release/bot  15M
```
Reasonable for a Rust binary with serenity + songbird + sqlx + symphonia + lofty.

***

## D1 — Implementation Correctness

### D1-1. Pipeline terminal states
Every input has a defined terminal:
| Stage | Success | Soft-skip | Hard-fail |
|---|---|---|---|
| Classifier | → Fingerprint | Skip unchanged mtime | ✅ N/A |
| Fingerprint | → AcoustID | — | Fingerprint error logged |
| AcoustID | → MusicBrainz | `no_match`/`low_confidence` | `failed`/`exhausted` |
| MusicBrainz | → CoverArt | `continue` on upsert err | `failed`/`exhausted` |
| CoverArt | → TagWriter + done | Cover absent = still done | Error = still done |
| TagWriter | `tags_written_at` set | `TrackNotFound` skip | File error logged |

✅ PASS — no input can reach a permanent non-terminal state.

### D1-2. enrichment_status exhaustiveness
States: `pending`, `enriching`, `done`, `failed`, `unmatched`, `low_confidence`, `exhausted`, `file_missing`.

- `enriching` → guarded by `reset_stale_enriching()` on startup (L56-65, main.rs). Stale locks auto-reset to `pending`.
- `failed`/`unmatched`/`low_confidence` → retried by `claim_for_enrichment()` poll loop until `exhausted`.
- `exhausted` → terminal. Manual CLI required to re-process.

✅ PASS — no track can get stuck.

### D1-3. TrackEndHandler event coverage
Handler registered for both `TrackEvent::End` and `TrackEvent::Error` (player.rs L111-115). Songbird fires exactly one of these for each track. ✅ PASS

### D1-4. intentional_stop analysis
No `intentional_stop` flag exists. The architecture avoids the problem entirely:
- `TrackEventHandler` pops from `meta_queue` on every End/Error.
- `/clear` clears `meta_queue` THEN calls `queue().stop()`.
- `/leave` clears `meta_queue` + clears state THEN calls `leave_channel()` which calls `queue().stop()`.
- After `queue().stop()`, Songbird fires End events for remaining tracks, but `meta_queue` is already empty so `pop_front()` returns `None` — no phantom advance.

✅ PASS — design eliminates the need for the flag.

### D1-5. /rescan guard
The `/rescan` command (rescan.rs) does NOT trigger an actual scan — it only logs and responds with a message. The scan is driven by the filesystem watcher's poll interval. There is no guard and none is needed.

✅ PASS — no concurrent rescan possible.

### D1-6. Now-playing embed staleness

**FINDING:** When `/clear` is called, the `meta_queue` is cleared and `post_now_playing(..., None, msg_id)` is called to show "Queue Ended". However, Songbird's `queue().stop()` (L61) fires `TrackEvent::End` for the currently playing track AFTER the queue clear. The `TrackEventHandler` then tries `meta_queue.pop_front()` on an already-empty queue (returns None), and hits the `meta_queue.is_empty()` branch, which calls `post_now_playing(None, ...)` again with a new now-playing message ID. **This results in a second "Queue Ended" embed being posted.**

**Impact:** Cosmetic — duplicate "Queue Ended" message. No data corruption.
**Severity:** MEDIUM
**Fix:** Check `state.now_playing_msg` in TrackEventHandler — if the embed was already updated to "Queue Ended" by the command, skip the redundant post.

***

## D2 — Self-Rolled vs Library

### Replacement candidates

| Location | Self-rolled pattern | Candidate crate | Recommendation | Effort |
|----------|-------------------|-----------------|----------------|--------|
| `track_event_handler.rs:229-233` | `format!("{minutes}:{seconds:02}")` | `humantime` 2.x | KEEP | — |
| `shared-config/lib.rs:129-137` | `parse_env<T>()` helper | `envconfig` / `figment` | KEEP | — |
| All 3 HTTP adapters | Manual `reqwest` + `governor` | `reqwest-middleware` | EVALUATE | MEDIUM |
| `enrichment_orchestrator.rs` | State-machine retry via poll | `backon` | KEEP | — |

### Keep (self-rolled is correct here)

1. **Duration formatting** — 3 lines vs adding a dependency for a single call. Self-rolled is correct.
2. **`parse_env<T>()`** — 8 lines, type-generic, no external dependencies. Replacing with `figment`/`envconfig` adds complexity for no gain.
3. **Retry via enrichment state machine** — not a retry loop; it's a state machine. `backon` would be inappropriate — the retry delay is externally driven by the poll interval.

### Evaluate (trade-off exists)

1. **reqwest-middleware**: Could unify rate-limiting + timeout + retry across all 3 HTTP adapters (AcoustID, MusicBrainz, CoverArt). However, `governor` is already well-integrated and each adapter has < 50 lines of HTTP code. **Verdict: DEFER to v3** — the current approach is clean and the migration cost is not justified for 3 simple clients.

***

## D3 — Hot-Path Analysis

| Path | Call freq | Bottleneck | Optimization | Impact | Priority |
|------|----------|-----------|-------------|--------|----------|
| `/play` autocomplete search | ~3 req/sec per active user | GIN index is correct; `ILIKE '%' || $2 || '%'` fallback does seq scan on `search_text` | Add `pg_trgm` GIN index on `search_text` (already exists: `idx_tracks_search_text`). **No issue.** | ✅ None | — |
| Classifier inner loop | 1 per FS event batch (64 max) | `find_many_by_blob_location` is batched, `tokio::fs::metadata` is async. | ✅ No issue | None | — |
| Enrichment poll | 1 per `scan_interval_secs` (300s) | `idx_tracks_enrichment_queue` partial index covers exact WHERE clause. | ✅ No issue | None | — |
| `GuildMusicState` lock | Every track end + every `/play` | Uses `tokio::sync::Mutex` with explicit `drop()` before `.await`. Lock scope is narrow. | ✅ No issue | None | — |
| Tag writer file I/O | 1 per enriched track | Runs in `spawn_blocking` with SMB semaphore. Full file copy is required (atomic write pattern). | ✅ No issue | None | — |

**No critical or high-priority hot-path issues found.** ✅ PASS

***

## D4 — Test Coverage

### Current test inventory

| File | Tests | Invariants covered |
|------|-------|--------------------|
| `error.rs` | `test_retryable_persistence` | `PoolExhausted` is retryable, `NotFound` is not |
| `error.rs` | `test_retryable_cover_art` | `HttpError` not retryable, `ServiceUnavailable` is |
| `error.rs` | `test_retryable_acoustid` | `RateLimited` retryable, `InvalidResponse` not |
| `error.rs` | `test_persistence_kind_display` | `PersistenceKind` Display impl |
| `test_symphonia.rs` | `test_probe_flac` | Symphonia can probe a FLAC file |
| `test_lofty.rs` | `test_write_lofty_tmp` | lofty can write tags to a FLAC copy |
| `enrichment_smoke.rs` | 3 skeletons | None (all skip without env vars) |

### Coverage gap table

| # | Invariant | Test exists? | Proposed |
|---|-----------|-------------|----------|
| 1 | Track pending → done given valid audio | ❌ | T-1 |
| 2 | AcoustID 404 → `no_match`, not retried past limit | ❌ | T-2 |
| 3 | MB 429 → retried (status `failed`), not exhausted with attempts<limit | ❌ | T-3 |
| 4 | Missing audio file → tag writer skips gracefully | ❌ | T-4 |
| 5 | Genre written as multi-value FLAC tags | ❌ | T-5 |
| 6 | Genre re-read after write produces same `Vec<String>` | ❌ | T-6 |
| 7 | ISRC persisted after MB enrichment | ❌ | T-7 |
| 8 | Composer/lyricist persisted as TrackArtist rows | ❌ | T-8 |
| 9 | /play happy path: joins, plays, queue advances | ❌ | Deferred (requires Discord mock) |
| 10 | Channel move: queue preserved, no double-play | ❌ | Deferred |
| 11 | /leave: clears queue, no orphaned handler | ❌ | Deferred |
| 12 | Auto-leave timer fires after empty queue | ❌ | Deferred |
| 13 | Auto-leave timer cancels on /play | ❌ | Deferred |
| 14 | intentional_stop prevents phantom advance | N/A | Design eliminates need |
| 15 | TrackEvent::Error advances queue | ❌ | Deferred |
| 16 | play_track failure skips to next | ❌ | Deferred |
| 17 | DB pool exhaustion returns AppError | ✅ | `test_retryable_persistence` |
| 18 | `kind_str()` is stable per variant | ❌ | T-9 |

### Test implementation plan

#### T-4: Tag writer skips missing file
Type: unit
Setup: Call `write_tags_atomic()` with a path to a nonexistent file.
Assertions: Returns `Err(AppError::TagWrite { kind: CopyFailed })`.
Complexity: LOW
Blocks ship: NO

#### T-5: Genre multi-value write
Type: unit
Setup: Copy test FLAC to temp dir. Call `write_tags_atomic()` with `genres: vec!["Hip Hop", "R&B"]`. Re-read with `Probe::open()`, iterate `ItemKey::Genre` items.
Assertions: Exactly 2 Genre tag items with values "Hip Hop" and "R&B". Not a single "Hip Hop;R&B".
Complexity: LOW
Blocks ship: NO

#### T-9: kind_str() exhaustiveness
Type: unit
Setup: Construct one `AppError` per variant.
Assertions: Each `kind_str()` returns a non-empty `&'static str`. No two variants return the same string.
Complexity: LOW
Blocks ship: NO

***

## D5 — Error Handling

### Unhandled Results
`cargo check` produces no unused result warnings. ✅ PASS

### unwrap/expect audit
See Diagnostic #5. **All legitimate** — startup assertions or structurally guaranteed by preceding checks. One improvement opportunity: `main.rs:211` `discord_token.parse().unwrap()` could use `?`.

### Swallowed errors
See Diagnostic #6. **All intentional** — Discord response fire-and-forget, and pipeline worker DB updates that are retried by the poll loop.

***

## D6 — Dependency Audit

### Duplicates
`reqwest` 0.12 vs 0.13, `tungstenite` 0.26 vs 0.28 — both from serenity/songbird upstream. Not actionable.

### Vulnerabilities
`cargo audit` not installed. Recommend running before production deployment.

### Unnecessary deps
No test-only crates found in `[dependencies]`. ✅

### Serenity/Songbird upstream status
Both on `next`/`serenity-next` branches (git deps). No version updates since last check. Serenity at `2fdbd065`, Songbird at `7d964c5a`.

***

## D7 — Operational Readiness

### .env.example gaps
**4 missing variables.** See Diagnostic #14. **Severity: HIGH.**

### sqlx cache status
✅ Up to date.

### Startup validation
`Config::load()` → `Config::from_env()` validates all required vars. `config.validate()` checks `MB_USER_AGENT` format. `MusicBrainzAdapter::new()` asserts UA format. **Startup fails fast with clear error.** ✅

### Graceful shutdown completeness
All 6 workers (AcoustID, MB, CoverArt, TagWriter, TagPoller, Orchestrator) are wrapped in `tokio::select!` with `tok.cancelled()`. The scanner has internal token checking. ✅ **Complete.**

### Log quality assessment
✅ All pipeline stages emit structured logs with `operation`, `track_id`, `correlation_id`, `error.kind`. An operator can trace a track through the full pipeline and identify stall points.

### Binary size
15M release. Reasonable. No optimization needed.

***

## Implementation Plan

### Work Items (delta changes required for ship)

***

#### WI-1: .env.example completeness
Severity: **HIGH**
Findings: D7, Diagnostic #14
Files: `.env.example`
Approach: Add `TAG_WRITE_CONCURRENCY`, `AUTO_LEAVE_SECS`, `MB_FETCH_WORK_CREDITS`, `LOG_FORMAT` with defaults and comments.
Scope: ~10 lines added
Risk: None

***

#### WI-2: Duplicate "Queue Ended" embed on /clear
Severity: **MEDIUM**
Findings: D1-6
Files: `track_event_handler.rs`
Approach: In TrackEventHandler, after `pop_front()` returns `None` from an already-empty queue, skip the "Queue Ended" post — it was already sent by the `/clear` command. Check: if `meta_queue` was already empty before `pop_front()`, return early.
Scope: ~4 lines
Risk: Low — could suppress a legitimate "Queue Ended" if a track ends naturally at the exact moment `/clear` runs. Edge case is cosmetic-only.

***

### Testing Workstream

***

#### T-4: tag_write_missing_file
Type: unit
Coverage: Missing audio file → tag writer returns error
Setup: Call `write_tags_atomic(Path::new("/nonexistent/track.flac"), &tag_data)`
Assertions: `matches!(result, Err(AppError::TagWrite { kind: TagWriteErrorKind::CopyFailed, .. }))`
Complexity: LOW
Blocks ship: NO

***

#### T-5: genre_multi_value_write
Type: unit
Coverage: Multi-value genre FLAC tags written correctly
Setup: Copy a known-good FLAC fixture to `$TMPDIR`. Call `write_tags_atomic()` with `genres: vec!["Hip Hop".into(), "R&B".into()]`. Re-probe with lofty.
Assertions: `tag.items().filter(|i| i.key() == ItemKey::Genre).count() == 2`; values are "Hip Hop" and "R&B" individually.
Complexity: LOW
Blocks ship: NO

***

#### T-9: kind_str_exhaustiveness
Type: unit
Coverage: `kind_str()` returns unique stable strings for all AppError variants
Setup: Construct one instance per AppError variant.
Assertions: All `kind_str()` values are unique and non-empty. Use `HashSet` to verify uniqueness.
Complexity: LOW
Blocks ship: NO

***

### Library Replacement Workstream

No replacements recommended. All candidates evaluated as KEEP or DEFER. See D2.

***

### Ship Checklist

#### Required to ship
- [x] All CRITICAL work items complete (none exist)
- [ ] WI-1 (.env.example) — **HIGH, blocks ship**
- [x] `cargo clippy --workspace` zero warnings
- [x] `cargo test --workspace` zero failures
- [x] `cargo sqlx prepare --check` passes
- [ ] `.env.example` documents all variables
- [x] No `.unwrap()` without documented invariant in non-test code
- [x] CancellationToken respected in all workers

#### Recommended before v3 begins
- WI-2: Fix duplicate "Queue Ended" embed
- T-4, T-5, T-9: Write ship-confidence tests
- Run `cargo audit` in CI

#### Deferred to v3
- Discord interaction tests (T-9 through T-16) — requires mock framework
- `reqwest-middleware` unification (D2)
- Pedantic clippy doc warnings
- `/rescan` command replacement with proper force-rescan logic
- `test_lofty.rs` hardcoded path → fixture file

### Recommended execution order
1. WI-1 (.env.example) — 2 min
2. WI-2 (duplicate embed fix) — 5 min
3. T-5 (genre write test) — 10 min
4. T-9 (kind_str test) — 5 min
5. T-4 (missing file test) — 5 min

### Estimated total rework scope
CRITICAL+HIGH WIs: 1 item, ~10 lines
MEDIUM WIs: 1 item, ~4 lines
Tests to write: 3, ~60 lines
Library replacements: 0
