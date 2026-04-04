# TeamTI v2 — Pass 8 Prompt
## Final Ship-Gate Audit: Correctness, Optimization & Test Finalization

> This is the last pass before v2 is considered complete and frozen.
> The agent reads the entire codebase, runs all diagnostics, and produces
> a SHIP / NO-SHIP verdict with a precise list of any remaining blockers.
> No findings are pre-enumerated. The agent audits freely and proposes
> its own implementation plan for any delta changes required.
> Nothing is applied without explicit human approval.

---

### Context

TeamTI v2 is a Discord music bot with a full NAS-backed metadata enrichment
pipeline. The full feature set entering Pass 8:

**Pipeline:**
Filesystem watcher → Classifier → Fingerprint Worker (Chromaprint + Symphonia)
→ AcoustID → MusicBrainz (recording + work credits) → Cover Art
→ Tag Writer (lofty, multi-value genres, extended fields)
→ PostgreSQL (sqlx, migrations, tsvector search)

**Discord:**
/play (autocomplete, queue, auto-join/leave), /clear, /leave, /rescan
Songbird voice playback, per-guild state machine, now-playing embed

**Cross-cutting:**
Unified AppError + kind_str() + Retryable trait (Pass 4.5)
Structured logging with correlation IDs (Pass 4.5)
CancellationToken graceful shutdown (Pass 2)
Shared config via environment variables

**Deferred to v3 (do not audit or implement):**
/pause, /resume, position-seeking, Redis queue persistence,
persistent now-playing message across restarts

---

### Audit Dimensions

Work through all seven dimensions. Give equal weight to each.
Where a dimension has no findings, write "PASS — no findings" and move on.

---

#### D1 — Implementation Correctness

Verify the final state of every pipeline stage against its intended behavior.
This is not a re-run of previous audits — focus on what could have regressed
or been introduced incorrectly during Passes 5–7:

1. Does every pipeline stage have a clearly defined terminal state for each
   input (success, soft-skip, hard-fail)?

2. Are all state transitions in `enrichment_status` exhaustive and
   deterministic? Can a track get stuck in a non-terminal state with no
   mechanism to advance?

3. Does the `TrackEndHandler` in `adapters-voice` correctly handle all
   Songbird event types that represent track completion (end, error)?
   Are there event types that could fire without the handler catching them?

4. Is `intentional_stop` (or equivalent flag) set and cleared atomically
   relative to all code paths that stop a track? Prove this by tracing
   every call to `TrackHandle::stop()` and `Call::stop()` across the codebase.

5. Is the `/rescan` command's scan guard (from Pass 2.1) still effective
   after the Pass 7 changes? Can two concurrent rescans now be triggered?

6. Does the now-playing embed always reflect the actual playback state?
   Identify any transition (track end, /clear, /leave, channel move, error)
   where the embed could show stale content.

---

#### D2 — Self-Rolled vs Library Analysis

This is a primary deliverable of Pass 8. Identify every implementation in
the codebase that reinvents functionality available in a well-maintained
Rust crate. For each finding, evaluate whether the replacement is worth
the migration cost.

Specific patterns to hunt for:

**Retry logic:**
Any manual `for i in 0..max_retries { ... sleep ... }` loop.
Candidate replacement: `backon` crate (exponential backoff, jitter,
configurable retry conditions, zero-cost abstraction over async).

**Rate limiting:**
Any hand-rolled token bucket, leaky bucket, or sleep-based throttle.
Candidate replacement: `governor` crate (if not already used),
or `tower::limit::RateLimitLayer` if a Tower middleware stack exists.

**HTTP middleware (retry + timeout + rate limit):**
If `reqwest` is used directly with manual retry and timeout handling,
consider `reqwest-middleware` + `reqwest-retry` for a composable approach.

**String normalization / Unicode handling:**
Any manual `.to_lowercase()` + `.trim()` chains used for fuzzy matching.
Candidate: `unicode-normalization` crate for NFD/NFC normalization,
`unidecode` for ASCII folding in search.

**Duration / time formatting:**
Any manual `format!("{:02}:{:02}", mins, secs)` for display.
Candidate: `humantime` for human-readable durations.

**HashMap in hot paths:**
Any `std::collections::HashMap` used in contexts that are called
frequently (per-track, per-scan-event, per-Discord-interaction).
Candidate: `ahash::AHashMap` for ~30% faster lookups via non-cryptographic
hashing (safe for non-adversarial inputs like track IDs and guild IDs).

**Base64 / URL encoding:**
Any manual percent-encoding or base64 for API parameters.
Candidate: `percent-encoding`, `base64` crates.

**Error context chaining:**
Any `map_err(|e| format!("failed to ...: {e}"))` that discards the
original error type. Should use `anyhow::Context` or
`.map_err(|e| AppError::SomeVariant { detail: e.to_string() })`.

**Concurrent iteration:**
Any sequential `for track in tracks { worker(track).await }` where
order does not matter. Candidate: `futures::stream::iter(...).buffer_unordered(N)`
for concurrent processing with a configurable concurrency limit.

For each found instance:
- Quote the self-rolled code
- Name the replacement crate and version
- State: REPLACE (clear win), EVALUATE (trade-off exists), KEEP (self-rolled is correct here)
- If REPLACE: provide the specific API call that replaces it

---

#### D3 — Hot-Path Analysis

Identify the five hottest code paths (called most frequently at runtime)
and evaluate each for unnecessary allocations, blocking operations,
synchronization overhead, or algorithmic inefficiency.

Candidates to investigate (verify which are actually hot):

1. **Scan classifier inner loop** — called once per filesystem event.
   Check for: unnecessary `String` clones, redundant DB reads for
   already-classified paths, lock contention on the seen-paths map.

2. **Enrichment status poll** — the query that finds `pending` tracks
   for each worker. Check for: missing index on `(enrichment_status, updated_at)`,
   full table scan, N+1 query patterns.

3. **tsvector search** (`/play` autocomplete) — called on every keystroke
   in the Discord autocomplete window (up to 1 req/300ms per active user).
   Check for: query plan (should use GIN index), result set size,
   unnecessary columns fetched.

4. **`GuildMusicState` lock contention** — every Songbird event and every
   Discord interaction acquires this Mutex. Check for: lock held across
   `.await` points, unnecessarily broad critical sections, data that could
   be moved outside the lock.

5. **Tag writer file I/O** — opens, modifies, and atomically renames audio
   files. Check for: unnecessary full-file reads when only tags need updating,
   missing `O_NOATIME` on read (for NAS mounts), sync overhead.

For each hot path:
- Confirm it is actually hot (estimate call frequency)
- Identify the specific bottleneck
- Propose the optimization with expected impact
- Classify: CRITICAL (bottleneck that limits system throughput),
  HIGH (measurable improvement), LOW (micro-optimization, defer to v3)

---

#### D4 — Final Correctness Tests

This is the test finalization pass. The goal is: after Pass 8, the test
suite gives sufficient confidence to ship v2 without manual verification
of the critical paths.

**Step 1: Current coverage audit**

Run:
```bash
cargo test --workspace 2>&1
find . -path "*/target" -prune -o -name "*.rs" -print \
    | xargs grep -l "#\[test\]\|#\[tokio::test\]\|#\[cfg(test)\]" 2>/dev/null
```

List every test that currently exists. For each test, classify:
- UNIT / INTEGRATION / END-TO-END
- What invariant it verifies
- Whether it would have caught any bug found in Passes 2.x–7.x

**Step 2: Coverage gap analysis**

For each of the following critical invariants, state whether a test
currently exists. If not, propose one:

| Invariant | Test exists? |
|---|---|
| Track in `pending` → progresses to `done` given valid audio file |  |
| Track with AcoustID 404 → marked `no_match`, not retried |  |
| Track with MB 429 → retried with backoff, not marked failed |  |
| Track with missing audio file → tag writer skips gracefully |  |
| Genre written as multi-value FLAC tags, not semicolon-joined |  |
| Genre re-read after write produces same Vec<String> |  |
| ISRC persisted after MB enrichment |  |
| Composer/lyricist persisted as TrackArtist rows |  |
| /play happy path: joins channel, plays audio, queue advances |  |
| Channel move: queue preserved, no double-play |  |
| /leave: clears queue, no orphaned TrackEndHandler |  |
| Auto-leave timer fires after empty queue |  |
| Auto-leave timer cancels when /play called during window |  |
| intentional_stop prevents phantom queue advance |  |
| TrackEvent::Error advances queue (does not stall) |  |
| play_track failure skips to next track |  |
| DB pool exhaustion: returns AppError, not panic |  |

**Step 3: Test implementation plan**

For each missing critical test, propose:
- Test type (unit / integration / e2e)
- Exact setup (fixture files, mock HTTP responses, test DB state)
- Specific assertions
- Infrastructure requirements
- Estimated complexity: LOW / MEDIUM / HIGH

---

#### D5 — Error Handling Completeness

Do a final sweep of the entire error surface:

1. Run `cargo check 2>&1 | grep "unused\|never used"` — every unused
   `Result` is a swallowed error.

2. Find every `.unwrap()` and `.expect()` in non-test code:
```bash
grep -rn "\.unwrap()\|\.expect(" --include="*.rs" \
    crates/ apps/ | grep -v "#\[test\]\|#\[cfg(test)\]\|target/"
```
   For each: is this a legitimate invariant (document with a comment)
   or a latent panic waiting to happen (replace with `?` or explicit handling)?

3. Find every `_ =` and `let _ =` that discards a Result:
```bash
grep -rn "let _ =\|_ =" --include="*.rs" crates/ apps/ \
    | grep -v "target/\|test\|//"
```
   For each: is the error intentionally discarded (fire-and-forget send,
   best-effort cleanup) or accidentally ignored?

4. Verify every `AppError` variant has a corresponding test that exercises
   the `kind_str()` output — this ensures alert rules keyed on `error.kind`
   values are never broken by a rename.

---

#### D6 — Dependency Audit

Do a final review of the dependency tree for security, duplication, and
unnecessary weight:

```bash
cargo tree --workspace --depth 2
cargo audit  # if cargo-audit is available
cargo outdated  # if cargo-outdated is available
```

1. Are there duplicate versions of any significant dependency
   (tokio, serde, sqlx, serenity, songbird)? Duplicates increase compile
   time and binary size.

2. Are there any dependencies with known vulnerabilities
   (`cargo audit` output)?

3. Are there any dependencies that are pulled in transitively but could
   be removed by feature-flagging a direct dependency?

4. Is `dev-dependencies` clean? Any test-only crate accidentally in
   `[dependencies]` inflates the production binary.

5. Are Serenity and Songbird still on the same git branch/rev as v1?
   Have there been upstream commits to those branches that fix known bugs?
   (Check commit log, note any fixes relevant to v2's issue history.)

---

#### D7 — Operational & Deployment Readiness

Final check that v2 can be deployed and operated:

1. **`.env.example`** — does it document every environment variable the
   bot reads? Is there a variable that was added in Passes 5–7 but not
   added to `.env.example`?

2. **Migration completeness** — do the migrations, applied in order,
   produce a schema that exactly matches the `sqlx` offline query cache?
   Run `cargo sqlx prepare --check --workspace`.

3. **Startup validation** — does `main.rs` validate all required config
   at startup (before any worker starts) and exit with a clear error
   message if a required variable is missing?

4. **Graceful shutdown completeness** — trace the CancellationToken signal
   through every worker. Is there any worker that does not select on
   the token and could run forever after shutdown is requested?

5. **Log output quality** — given only the structured JSON log, can an
   operator answer these questions without reading source code:
   - Which track is currently being enriched and at which stage?
   - Why did the last enrichment run stall?
   - Which guild's queue is currently playing and what track?
   - When was the last successful tag write?

6. **Binary size and startup time** — run:
   ```bash
   cargo build --release --workspace
   ls -lh target/release/teamti-bot
   time target/release/teamti-bot --check-config  # if such a flag exists
   ```
   Note: for a NAS-hosted bot, these are informational, not blocking.

---

### Full Diagnostic Suite

Run every command. Include full output (truncated at 80 lines if very long).

```bash
# 1. Release build — zero warnings, zero errors
cargo build --release --workspace 2>&1

# 2. Full test suite
cargo test --workspace 2>&1

# 3. Clippy — zero warnings, pedantic level
cargo clippy --workspace --all-targets -- -W clippy::pedantic \
    -A clippy::module_name_repetitions \
    -A clippy::must_use_candidate 2>&1

# 4. Dead code sweep
cargo check --workspace 2>&1 \
    | grep -E "warning: (unused|dead_code|never used|never constructed)"

# 5. unwrap / expect in non-test code
grep -rn "\.unwrap()\|\.expect(" --include="*.rs" \
    crates/ apps/ | grep -v "target/\|#\[cfg(test)\]\|mod tests"

# 6. Swallowed results
grep -rn "let _ =\|= _ " --include="*.rs" \
    crates/ apps/ | grep -v "target/\|//\|test"

# 7. TODO/FIXME/HACK/STUB sweep
grep -rn "todo!()\|unimplemented!()\|// TODO\|// FIXME\|// HACK\|// STUB" \
    --include="*.rs" . | grep -v "target/"

# 8. Self-rolled retry patterns
grep -rn "for.*retries\|retry_count\|max_attempts\|sleep.*retry" \
    --include="*.rs" crates/ apps/ | grep -v "target/\|test"

# 9. HashMap in hot paths
grep -rn "HashMap::new\|BTreeMap::new" --include="*.rs" \
    crates/ apps/ | grep -v "target/\|test"

# 10. Blocking calls in async context
grep -rn "std::thread::sleep\|std::fs::\|std::io::Read" \
    --include="*.rs" crates/ apps/ | grep -v "target/\|test\|//"

# 11. .await on MutexGuard (deadlock risk)
grep -n "lock().*await\|lock().unwrap().*await" \
    --include="*.rs" -r crates/ apps/ | grep -v target/

# 12. Dependency duplicates
cargo tree --workspace | sort | uniq -d | head -20

# 13. sqlx cache freshness
cargo sqlx prepare --check --workspace 2>&1

# 14. .env.example completeness
cat .env.example 2>/dev/null || echo "NO .env.example FOUND"
# then compare against all std::env::var() calls:
grep -rn "std::env::var\|env::var\|dotenvy" \
    --include="*.rs" crates/ apps/ | grep -v "target/\|test"

# 15. Migration ordering check
ls -1 crates/adapters-persistence/migrations/

# 16. Test file inventory
find . -path "*/target" -prune -o -name "*.rs" -print \
    | xargs grep -l "#\[test\]\|#\[tokio::test\]" 2>/dev/null

# 17. CancellationToken usage in every worker
grep -rn "CancellationToken\|cancelled()\|cancel()" \
    --include="*.rs" crates/ apps/ | grep -v "target/"

# 18. Binary size
ls -lh target/release/teamti* 2>/dev/null || echo "Release build not yet run"
```

---

### Report Format

```markdown
# Pass 8 Final Audit Report

## VERDICT: SHIP / NO-SHIP
[State this at the top. List every blocker if NO-SHIP.]

***

## Diagnostic Output
[per-command output, 1–18]

***

## D1 — Implementation Correctness
[per-finding: Evidence / Impact / Severity]

## D2 — Self-Rolled vs Library

### Replacement candidates
| Location | Self-rolled pattern | Candidate crate | Recommendation | Effort |
|----------|--------------------|--------------------|----------------|--------|

### Approved replacements (implement immediately)
[list with specific API substitution]

### Evaluate (trade-off exists)
[list with trade-off notes]

### Keep (self-rolled is correct here)
[list with justification]

## D3 — Hot-Path Analysis

### Identified hot paths (ranked by frequency)
| Path | Call frequency | Bottleneck | Optimization | Impact | Priority |
|------|---------------|-----------|--------------|--------|----------|

## D4 — Test Coverage

### Current test inventory
| File | Tests | Invariants covered |

### Coverage gap table
[per-invariant: exists? / proposed test if missing]

### Test implementation plan
[T-N format: type / setup / assertions / complexity]

## D5 — Error Handling
### Unhandled Results
### unwrap/expect audit
### Swallowed errors

## D6 — Dependency Audit
### Duplicates
### Vulnerabilities (cargo audit)
### Unnecessary deps
### Serenity/Songbird upstream status

## D7 — Operational Readiness
### .env.example gaps
### sqlx cache status
### Startup validation
### Graceful shutdown completeness
### Log quality assessment

***

## Implementation Plan

### Work Items (delta changes required for ship)
***
#### WI-N: <title>
Severity: CRITICAL / HIGH / MEDIUM
Findings: [IDs]
Files: [list]
Approach: <specific steps>
Dependencies: [other WIs]
Scope: <lines>
Risk: <what could go wrong>
***

### Testing Workstream
***
#### T-N: <test name>
Type: unit / integration / e2e
Coverage: <invariant>
Setup: <fixtures, mocks, DB state>
Assertions: <specific>
Complexity: LOW / MEDIUM / HIGH
Blocks ship: YES / NO
***

### Library Replacement Workstream
***
#### LR-N: <replacement>
Crate: <name + version>
Replaces: <file:line>
Substitution: <exact API>
Complexity: LOW / MEDIUM / HIGH
***

### Ship Checklist

#### Required to ship
- [ ] All CRITICAL and HIGH work items complete
- [ ] All ship-blocking tests passing
- [ ] `cargo clippy --workspace` zero warnings
- [ ] `cargo test --workspace` zero failures
- [ ] `cargo sqlx prepare --check` passes
- [ ] `.env.example` documents all variables
- [ ] No `.unwrap()` without documented invariant in non-test code
- [ ] CancellationToken respected in all workers

#### Recommended before v3 begins
[list]

#### Deferred to v3
[list]

### Recommended execution order
[numbered sequence accounting for dependencies]

### Estimated total rework scope
CRITICAL+HIGH WIs: N items, ~N lines
Tests to write: N, ~N lines
Library replacements: N, ~N lines
```

---

### Constraints

- Do not apply any changes without approval.
- Exception: if `cargo build --release` or `cargo test` fails, fix the
  minimum necessary to unblock diagnostics and note exactly what was changed.
- D2 (library replacements) must not increase compile times significantly
  for a replacement crate that is only marginally better.
- Test proposals must be concrete — exact fixture files, mock payloads,
  SQL state, and assertions. Vague test descriptions are rejected.
- The SHIP / NO-SHIP verdict must be the first line of the report.
  If NO-SHIP, every blocker must have a corresponding work item.
- Be adversarial. The goal is to find problems, not to confirm the
  implementation is fine. If something looks correct, verify it is
  correct — do not assume.

---