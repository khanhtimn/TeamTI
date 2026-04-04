# TeamTI v2 — Pass 6 Prompt
## Full-Stack Audit, Optimization & Test Coverage

> This is the final audit pass before v2 is considered shippable.
> The agent reads the entire codebase, runs diagnostics, and produces
> an independent assessment and a prioritized implementation plan.
> No findings are pre-enumerated. No solutions are prescribed.
> The agent proposes; the human approves before any code changes.

---

### Context

TeamTI v2 is a Discord music bot backed by a NAS-hosted audio library.
The full pipeline is:

```
NAS filesystem
    → PollWatcher (notify)
    → Classifier (mtime/size dedup)
    → Fingerprint Worker (Chromaprint + Symphonia, SMB semaphore)
    → Enrichment Orchestrator (poll DB for pending)
    → AcoustID Worker (rate-limited HTTP)
    → MusicBrainz Worker (rate-limited HTTP)
    → Cover Art Worker (HTTP + lofty embedded art)
    → Tag Writer Worker (atomic lofty writeback)
    → PostgreSQL (sqlx, migrations)
    → TrackSearchPort (tsvector full-text)
    → Discord commands (/play /clear /leave /rescan)
    → Songbird voice playback
    → Now-playing embeds
```

**What is in scope for v2 (implemented across Passes 1–5.x):**
- Full scan and enrichment pipeline
- Atomic tag writeback
- Unified error handling (AppError, Pass 4.5)
- Structured logging and correlation IDs (Pass 4.5)
- Discord slash commands: /play (with autocomplete), /clear, /leave, /rescan
- Songbird voice playback with queue management
- Auto-leave timer
- Now-playing embed with auto-update

**What is explicitly deferred to v3:**
- /pause, /resume
- Position-seeking on channel move
- Persistent now-playing message (across restarts)
- Redis-backed queue persistence

The goal of this pass is to determine: **is v2, as implemented, correct,
robust, and testable enough to ship?** If not, what is the minimum delta
to make it so?

---

### Audit Dimensions

Evaluate the codebase across six dimensions. Each dimension is equally
important — do not skip any.

---

#### D1 — Pipeline Correctness

For each stage in the pipeline, verify:
- The happy path produces the expected database state and side effects
- Failures at that stage are handled and do not corrupt state in adjacent stages
- The stage can be restarted (bot restart) without reprocessing already-complete work
- The stage does not block the pipeline indefinitely on a single bad input

Specific questions to answer independently:
1. If the bot crashes between AcoustID matching and MusicBrainz fetching,
   what happens to that track on restart? Is the acoustid_id persisted?
2. If a track's audio file is deleted from the NAS after it is indexed,
   what happens when the Tag Writer tries to write its tags?
3. If MusicBrainz returns a 404 for a given MBID, does the track get
   correctly marked so it is not retried indefinitely?
4. Can the enrichment pipeline process the same track concurrently
   from two claims? What prevents this?
5. What is the behavior if the PostgreSQL connection pool is exhausted
   under a large initial library scan?

---

#### D2 — Architecture & Design

Evaluate the overall design for soundness:

1. **Hexagonal architecture adherence:** Are all infrastructure concerns
   (DB, HTTP, filesystem, Discord) strictly behind port interfaces?
   Are there any direct infrastructure calls from the application layer?

2. **Channel topology:** Are channel capacities appropriate for the
   expected throughput? Are there any unbounded channels? Are there
   any back-pressure gaps where a fast producer can silently drop messages?

3. **Shared state:** Is shared mutable state minimized? Where it exists,
   is it correctly protected? Is there shared state that should instead
   be passed through message channels?

4. **Crate boundaries:** Does each crate have a clear single responsibility?
   Are there circular dependencies (check `cargo tree`)?
   Are there crates that have grown beyond their mandate?

5. **Configuration:** Is all runtime configuration accessible via
   `shared-config`? Are there any hardcoded values that should be
   configurable (timeouts, retry limits, concurrency levels)?

6. **Error propagation:** Does every error that reaches a user (Discord
   response or log line) contain enough context to diagnose the problem
   without reading source code?

---

#### D3 — Robustness & Edge Cases

Investigate how the system behaves under adverse conditions:

1. **NAS unavailability:** What happens if the SMB mount disappears
   mid-scan? Mid-enrichment? Mid-playback? Does the bot recover
   when the mount comes back?

2. **Rate limit exhaustion:** If AcoustID or MusicBrainz returns 429
   for an extended period, does the enrichment pipeline back off
   correctly without hammering the API?

3. **Large library:** With 50,000+ tracks, are there any O(n) operations
   at startup or per-scan that will cause timeouts or memory pressure?

4. **Duplicate file detection:** If the same audio file appears at two
   different paths (symlinks, re-rips), does the pipeline handle it
   gracefully or create duplicate tracks?

5. **Concurrent Discord users:** If three users call /play simultaneously
   in the same guild, what is the state machine behavior?
   Is there a race condition in queue management?

6. **Discord API rate limits:** Does the bot handle Discord HTTP 429
   responses on embed updates without crashing or losing the
   now-playing message reference?

---

#### D4 — Performance

Evaluate throughput and resource usage:

1. **Initial scan throughput:** What is the theoretical maximum tracks/hour
   for the full pipeline (fingerprint → enrich → tag write) given the
   current concurrency settings and rate limits?

2. **Database query efficiency:** Run `EXPLAIN ANALYZE` mentally against
   the five most frequent queries. Are all necessary indexes present?
   Are any queries doing sequential scans on large tables?

3. **Memory profile:** At steady state with 50,000 tracks in the DB and
   a 10-track queue playing, what is the approximate resident memory
   of the bot process?

4. **Channel backpressure:** Are bounded channels sized appropriately
   relative to the throughput of their consumers? Identify any channel
   where the producer is likely to block waiting for the consumer.

5. **Connection pool sizing:** Is the DB pool size appropriate for the
   number of concurrent async tasks that may be waiting on a query?

---

#### D5 — Test Coverage

This is a primary deliverable of Pass 6. Evaluate the current test
situation honestly, then propose a testing plan.

**Existing tests audit:**
1. List every test file that currently exists in the workspace
2. For each: what does it cover? What does it NOT cover?
3. Which R1 flows from Pass 5.2 have zero test coverage?
4. Which pipeline stages have zero test coverage?

**Missing critical tests:**
Identify the tests that, if they existed, would give the highest confidence
that v2 is correct. Prioritize by:
- Tests that would have caught bugs found in audit passes 2.x–5.x
- Tests for the enrichment pipeline state machine (claim, fail, exhaust, done)
- Tests for the queue management state machine (idle, playing, multi-track, channel-move)
- Tests for error recovery paths (bad file, API 404, API 429, DB timeout)

**Test implementation plan:**
For each missing critical test, propose:
- Test type: unit / integration / end-to-end
- What it tests
- What infrastructure it requires (test DB, mock HTTP, fixture audio files)
- Estimated complexity: LOW / MEDIUM / HIGH

---

#### D6 — Operational Readiness

Evaluate whether the bot can be operated in a real environment:

1. **Startup sequence:** Is the startup order safe? (DB migrations before
   query execution, config validation before any service starts, etc.)

2. **Graceful shutdown:** When the CancellationToken fires, do all workers
   drain their in-flight work before exiting? Or do they drop messages?

3. **Log quality:** Given only the structured log output (JSON format),
   can an operator diagnose the following scenarios without reading code:
   - A track that has been stuck in `enriching` for 24 hours
   - A voice channel join failure
   - A DB connection pool exhaustion

4. **Migration safety:** Are all migrations reversible (have `DOWN` scripts)?
   Can migrations be applied to a live database without locking the
   `tracks` table for an extended period?

5. **Secret management:** Are all secrets (API keys, Discord token, DB URL)
   read from environment variables and never logged, even at DEBUG level?

6. **Restart safety:** If the bot is killed with SIGKILL (no graceful shutdown),
   can it restart cleanly with no manual intervention required?

---

### Full File Inventory

Read every file in the following crates before writing any findings:

```
Cargo.toml                          (workspace manifest)
.env.example                        (if present)

crates/domain/src/
crates/application/src/
crates/shared-config/src/
crates/adapters-persistence/src/
crates/adapters-persistence/migrations/
crates/adapters-watcher/src/
crates/adapters-media-store/src/
crates/adapters-acoustid/src/
crates/adapters-musicbrainz/src/
crates/adapters-cover-art/src/
crates/adapters-voice/src/
crates/adapters-discord/src/
apps/bot/src/
```

Also read every test file in the workspace:
```bash
find . -name "*.rs" | xargs grep -l "#\[test\]\|#\[tokio::test\]\|#\[cfg(test)\]"
```

---

### Diagnostic Commands

Run all of the following. Include full output (truncated at 100 lines if very
long) in the report under "Diagnostic Output":

```bash
# 1. Full build — zero warnings required
cargo build --workspace 2>&1

# 2. All tests
cargo test --workspace 2>&1

# 3. Dead code sweep (entire workspace)
cargo check --workspace 2>&1 | grep -E \
    "warning: (unused|dead_code|never used|never constructed|unreachable)"

# 4. TODO/FIXME/STUB sweep (entire workspace)
grep -rn "todo!()\|unimplemented!()\|// TODO\|// FIXME\|// STUB\|// HACK" \
    --include="*.rs" . | grep -v "target/"

# 5. Hardcoded values that should be config
grep -rn "[0-9]\{2,\}" --include="*.rs" \
    crates/application/src/ crates/adapters-*/src/ \
    | grep -v "test\|migration\|uuid\|timestamp\|//\|#\[" \
    | grep -E "= [0-9]{2,}|Semaphore::new\([0-9]|channel\([0-9]|sleep.*[0-9]{2,}"

# 6. Circular dependency check
cargo tree --workspace 2>&1 | grep -E "^\[" | sort | uniq -d

# 7. Unbounded channels
grep -rn "unbounded\|channel()" --include="*.rs" \
    crates/ apps/ | grep -v "target/\|//\|test"

# 8. Direct infrastructure calls from application layer
grep -rn "sqlx::\|reqwest::\|notify::\|songbird::\|serenity::" \
    --include="*.rs" crates/application/src/

# 9. Secrets in log statements
grep -rn "api_key\|token\|password\|secret\|API_KEY\|TOKEN" \
    --include="*.rs" crates/ apps/ \
    | grep -iE "info!\|debug!\|warn!\|error!\|println!\|tracing"

# 10. Missing indexes — look for WHERE clauses without obvious index coverage
grep -rn "WHERE\|ORDER BY\|GROUP BY" \
    --include="*.rs" crates/adapters-persistence/src/ \
    | grep -v "test\|//"

# 11. Migrations without CONCURRENTLY on index creation
grep -rn "CREATE INDEX" \
    crates/adapters-persistence/migrations/

# 12. sqlx offline cache freshness
ls -la .sqlx/ 2>/dev/null || echo "No .sqlx offline cache found"
cargo sqlx prepare --check --workspace 2>&1 | tail -5

# 13. Full dependency tree for serenity, songbird, tokio
cargo tree | grep -E "serenity|songbird|tokio " | head -20

# 14. Test file inventory
find . -path "*/target" -prune -o -name "*.rs" -print \
    | xargs grep -l "#\[test\]\|#\[tokio::test\]\|#\[cfg(test)\]" 2>/dev/null

# 15. Enrichment status transitions — verify all paths lead to terminal state
grep -rn "EnrichmentStatus::\|enrichment_status" \
    --include="*.rs" crates/application/src/
```

---

### Report Format

The report has two parts: **Findings** and **Implementation Plan**.
The Implementation Plan is the primary deliverable — findings exist to
feed it.

```markdown
# Pass 6 Audit Report

---

## Part 1: Diagnostic Output

### 1. Build output
### 2. Test results
### 3. Dead code warnings
### 4. TODO/FIXME inventory
### 5. Hardcoded values
### 6. Circular dependencies
### 7. Unbounded channels
### 8. Infrastructure calls from application layer
### 9. Secrets in logs
### 10. Query/index review
### 11. Migration index safety
### 12. sqlx cache freshness
### 13. Dependency tree
### 14. Test file inventory
### 15. Enrichment status paths

---

## Part 2: Findings by Dimension

For each finding use this format:
**[ID] Title**
- Dimension: D1–D6
- Severity: CRITICAL / HIGH / MEDIUM / LOW
- Evidence: <code quote, query, or diagnostic output>
- Impact: <what breaks or degrades if not fixed>
- Proposed fix: <specific, actionable>

### D1 — Pipeline Correctness
[findings]

### D2 — Architecture & Design
[findings]

### D3 — Robustness & Edge Cases
[findings]

### D4 — Performance
[findings]

### D5 — Test Coverage

#### Existing tests
| File | What it covers | What is missing |
|------|---------------|-----------------|

#### Missing critical tests
| ID | Stage/Flow | Type | Infrastructure needed | Complexity |
|----|-----------|------|----------------------|------------|

### D6 — Operational Readiness
[findings]

---

## Part 3: Implementation Plan

### Scoring
Total findings: N
  CRITICAL: N  (must fix — v2 cannot ship with these)
  HIGH: N      (should fix — significant risk if deferred)
  MEDIUM: N    (polish — address before v3 work begins)
  LOW: N       (nice to have)

### Work Items

Group related findings into discrete work items.
Order by: CRITICAL first, then HIGH, then by dependency.

For each work item:

---
#### WI-N: <Short title>
**Findings addressed:** [IDs]
**Severity:** CRITICAL / HIGH / MEDIUM / LOW
**Affected files:** [list]
**Description:** <what needs to change and why>
**Implementation approach:** <specific steps the implementing agent should take>
**Dependencies:** <other work items that must be completed first>
**Estimated scope:** <lines changed, files touched>
**Risk:** <what could go wrong during implementation>
---

### Testing Workstream

List the tests to be written as a separate workstream.
Order by: tests that cover CRITICAL findings first.

For each test:
---
#### T-N: <Test name>
**Type:** unit / integration / end-to-end
**Coverage:** <what it verifies>
**Infrastructure:** <test DB / mock HTTP / fixture files / live Discord>
**Implementation notes:** <specific setup, assertions, edge cases>
**Estimated complexity:** LOW / MEDIUM / HIGH
---

### Minimum Shippable v2 Definition

Given all findings, define the minimum set of work items and tests that
must be completed for v2 to be considered shippable. State explicitly:

1. Work items that are REQUIRED for ship
2. Work items that are RECOMMENDED but not blocking
3. Work items that can safely defer to v3
4. Tests that are REQUIRED for ship
5. Tests that are RECOMMENDED but not blocking

### Recommended Execution Order

Provide a sequenced list of work items and tests in the order the
implementing agent should tackle them, accounting for dependencies
and risk. Format as a numbered list.
```

---

### Constraints

- Do not apply any changes in this pass. This is a report-and-plan pass only.
- Exception: if `cargo build --workspace` fails, fix the minimum necessary
  to unblock diagnostics, and note exactly what was changed and why.
- The Implementation Plan is a proposal. No work item is approved until
  the human reviews the report and explicitly approves items to proceed.
- The Testing Workstream is a first-class deliverable, not an appendix.
  It must be as detailed as the work items.
- Be honest about uncertainty. If a finding requires runtime observation
  to confirm (e.g. a suspected race condition that cannot be proven by
  static analysis alone), say so and recommend a targeted test to confirm.
- Do not pad the report. If a dimension has no findings, write
  "No findings — requirement satisfied" and move on.

---

### REFERENCE

*[Attach full `teamti_v2_master.md`, the v1 git refs for serenity and
songbird, and the `.env.example` file before sending to agent.]*
