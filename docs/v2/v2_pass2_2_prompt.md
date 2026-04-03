# TeamTI v2 — Pass 2.2 Prompt
## Reflection, Correctness Audit & Performance Review

> This is a **review-only pass**. No new features are added.
> The agent reads the Pass 2.1 implementation, identifies bugs and
> performance issues, and produces targeted fixes with justification.
> Every change must cite a specific finding from this checklist.

---

### Objective

Pass 2.1 introduced six new modules across four crates. Before Pass 3 builds
on top of them, this pass ensures the foundation is correct and efficient.
The primary concern is silent correctness failures — code that compiles,
runs, and even passes tests while producing wrong behavior under specific
conditions that are normal in production (e.g. concurrent scan cycles,
identical audio content at two paths, SMB latency spikes).

---

### Agent Instructions

1. Read each file listed in the **File Inventory** section in full.
2. Work through each item in the **Audit Checklist** in order.
3. For each item: confirm whether the issue exists, determine severity,
   and either fix it or document why it is acceptable.
4. Produce a **Findings Report** at the end (format defined in the last
   section of this prompt).
5. Apply all Critical and High fixes directly to the codebase.
   Do not defer them. Medium and Low items may be documented only if
   a fix would require structural changes beyond this pass scope.

---

### File Inventory

Read every one of these files before starting the checklist:

```
crates/adapters-watcher/src/watcher.rs
crates/adapters-watcher/src/event.rs
crates/adapters-media-store/src/classifier.rs
crates/adapters-media-store/src/tag_reader.rs
crates/adapters-media-store/src/fingerprint.rs
crates/adapters-media-store/src/scanner.rs
crates/adapters-media-store/Cargo.toml
crates/adapters-persistence/src/track_repository.rs   (or equivalent)
crates/application/src/events.rs
crates/application/src/enrichment_orchestrator.rs
apps/bot/src/main.rs
```

Also read:
```
crates/adapters-persistence/migrations/20250002000000_core_tables.sql
crates/adapters-persistence/migrations/20250004000000_indexes.sql
```

---

### Audit Checklist

Work through each item. Label each finding: CONFIRMED / NOT PRESENT / N/A.
For CONFIRMED findings, apply the fix unless severity is Low and the fix
would require out-of-scope structural changes.

---

#### SECTION A — Critical: Data Integrity

**A1. Duplicate fingerprint race condition (INSERT without conflict handling)**

Scenario: two files with identical audio content exist in `MEDIA_ROOT` at
the time of the first scan (or arrive in the same poll cycle). Both pass
through the Classifier (no dedup there), both are sent to the Fingerprint
Worker, both compute the same Chromaprint fingerprint. The first task calls
`find_by_fingerprint` → finds nothing → calls `insert`. The second task
calls `find_by_fingerprint` between the first task's `find` and `insert`
(no transaction) → also finds nothing → also calls `insert`. The second
INSERT violates the unique index `idx_tracks_fingerprint` and the
`insert()` call returns an `Err(sqlx::Error::Database)`.

Check: does `TrackRepositoryImpl::insert` use `ON CONFLICT DO NOTHING`
or `ON CONFLICT (audio_fingerprint) DO UPDATE`? If not, the Fingerprint
Worker will log a spurious error for every duplicate file.

Fix required: the INSERT for tracks must handle the fingerprint uniqueness
conflict gracefully. The correct behavior is:
- If a row with the same `audio_fingerprint` already exists: update
  `blob_location`, `file_modified_at`, `file_size_bytes` (same as the
  "file moved" path) and return the existing row without emitting
  `TrackScanned` again (already queued or already enriched).
- Use `ON CONFLICT (audio_fingerprint) DO UPDATE SET ...` in the INSERT,
  then inspect `xmax` or use `RETURNING` to determine if it was an insert
  or an update, to decide whether to emit `TrackScanned`.

Alternatively: wrap `find_by_fingerprint` + `insert` in a single
`INSERT ... ON CONFLICT ... RETURNING id, (xmax = 0) AS inserted` query.
This is atomic and eliminates the race entirely.

Severity: **Critical** — silent data duplication attempt under normal
multi-file scan conditions.

---

**A2. claim_for_enrichment transaction isolation**

Check: is the `BEGIN` / SELECT FOR UPDATE SKIP LOCKED / UPDATE / COMMIT
sequence all executed on the same database connection? In sqlx, `pool.begin()`
checks out a connection and holds it for the transaction. Verify that
`fetch_all(&mut *tx)` and `execute(&mut *tx)` both use the transaction
handle `tx`, not `&self.pool`. If any query in the sequence uses
`&self.pool` directly, it runs outside the transaction and the FOR UPDATE
lock is not honored.

Severity: **Critical** — lock escapes transaction boundary; concurrent
orchestrator instances (possible after hot restart) could double-claim rows.

---

**A3. enriching status leak on Fingerprint Worker panic**

Scenario: `claim_for_enrichment` sets rows to `enriching`. If the bot
process is killed between the DB claim and the AcoustID call (which happens
in Pass 3), rows stay in `enriching` permanently — invisible to users,
never retried, never reset.

Check: does the startup sequence call `reset_stale_enriching()` BEFORE
the pipeline tasks are spawned? If the pipeline starts before the watchdog
runs, a newly claimed batch could be immediately reset to `pending`.

The correct order in `apps/bot/main.rs` startup is:
1. Run migrations
2. Call `reset_stale_enriching()` and await its result
3. THEN spawn pipeline tasks

Verify this order is correct. If steps 2 and 3 are reversed or concurrent,
fix them.

Severity: **Critical** — startup ordering affects correctness on restart.

---

#### SECTION B — High: Correctness

**B1. AtomicBool ScanGuard drop ordering in watcher callback**

The `_guard = ScanGuard(...)` binding must outlive the last `blocking_send`
call. Clippy may have warned about the `_guard` binding and the
implementation may have changed it to `_ = ScanGuard(...)` (an anonymous
binding), which drops immediately — clearing the flag before forwarding
completes.

Check: is `_guard` a named binding (`let _guard = ...`) or an anonymous
discard (`let _ = ...`)? A named binding starting with `_` is NOT dropped
at the end of the statement — it lives to end of scope. An anonymous `_`
IS dropped immediately.

If `_ = ScanGuard(...)` is found anywhere, this is a correctness bug.
The flag clears before the event forwarding loop finishes, allowing a
concurrent poll cycle to start.

Fix: ensure `let _guard = ScanGuard(...);` with a leading underscore and
a name — never `let _ = ...`.

Severity: **High** — overlap guard becomes a no-op; concurrent scan cycles
re-enabled silently.

---

**B2. to_relative() called twice per file event**

In the current design, `to_relative()` is called in the Classifier
(to build the `rel` string for the DB lookup) and again in the Fingerprint
Worker (to build `rel` for the INSERT/UPDATE). The two calls must produce
identical output for the same path — but since the implementation uses
`strip_prefix(media_root)`, any difference in how `media_root` is stored
in config (trailing slash vs. none, canonicalized vs. not) could cause
the two calls to produce different strings.

Check: are both `to_relative()` calls using the same `config.media_root`?
Is `media_root` canonicalized at parse time in `shared-config`? If not,
a path like `/mnt/music` and `/mnt/music/` could produce different relative
paths.

Fix: canonicalize `media_root` once at startup using
`std::fs::canonicalize()` and store the canonical form in `Config`.
Alternatively, pass `rel: String` through the `ToFingerprint` struct
so it is computed exactly once in the Classifier and reused in the
Fingerprint Worker with no risk of divergence.

The second approach (pass `rel` through `ToFingerprint`) is preferred —
it removes the redundant computation and eliminates the divergence risk.

Add `rel: String` to `ToFingerprint`:
```rust
pub struct ToFingerprint {
    pub path:        PathBuf,
    pub rel:         String,   // ← pre-computed in Classifier
    pub mtime:       SystemTime,
    pub size_bytes:  u64,
    pub existing_id: Option<uuid::Uuid>,
}
```

Severity: **High** — silent path mismatch could cause every file to be
treated as a new track on every scan cycle.

---

**B3. Missing tokio-util in adapters-media-store Cargo.toml**

`scanner.rs` uses `tokio_util::sync::CancellationToken` but the Cargo.toml
additions in Pass 2.1 do not include `tokio-util`. This is a compile error.

Check: does `adapters-media-store/Cargo.toml` include
`tokio-util = { workspace = true, features = ["rt"] }`?

Fix: add it if missing. Also ensure it is in `[workspace.dependencies]`
in the root `Cargo.toml`.

Severity: **High** — build fails.

---

**B4. Fingerprint Worker emits TrackScanned for moved files**

In the "file moved" branch of the Fingerprint Worker, the current code
updates `blob_location` via `update_file_identity` and then falls through
WITHOUT emitting `TrackScanned`. This is correct — a moved file does not
need re-enrichment.

Check: confirm there is no `scan_tx.send(TrackScanned {...}).await` in
the "same audio, different path" branch. If there is, remove it.

If a moved, already-enriched (`status = 'done'`) track is re-emitted to
the enrichment queue, the Orchestrator will claim it, set it to
`'enriching'`, and queue an AcoustID call — wasting an API request and
temporarily hiding the track from users (status != 'done').

Severity: **High** — moved tracks become temporarily invisible.

---

**B5. Classifier DB lookup for every file on every poll cycle**

The Classifier calls `find_by_blob_location` once per file event. With
`PollWatcher` at a 300-second interval over a 20,000-track library, each
poll cycle generates up to 20,000 sequential DB round trips in the worst
case (all files modified). In practice most are hits that return quickly,
but the round-trip latency still dominates.

This is a performance issue, not a correctness issue. However, it becomes
a correctness-adjacent issue if the Classifier processes so slowly that
the next poll cycle begins before the Classifier has drained the previous
cycle's events — at which point the watcher's overlap guard (which only
guards event forwarding, not Classifier processing) no longer prevents
effective queue growth.

Recommended fix: add a batch lookup method to `TrackRepository`:

```rust
async fn find_many_by_blob_location(
    &self,
    locations: &[String],
) -> Result<HashMap<String, Track>, AppError>;
```

Implementation:
```sql
SELECT * FROM tracks
WHERE blob_location = ANY($1)
```

In the Classifier, collect all events from a poll cycle into a batch
(use `recv_many()` with a timeout), bulk-fetch from DB, then process
in-memory. This reduces 20,000 DB round trips to 1 per poll cycle.

Severity: **High** for libraries > 5,000 tracks. Medium for smaller
libraries. Implement the batch method now; wire it into the Classifier
in this pass.

---

#### SECTION C — Medium: Robustness

**C1. Symphonia decode: unhandled codec reset mid-stream**

The `ResetRequired` arm in `tag_reader.rs` calls `decoder.reset()` and
continues. However, after a reset, the decoder may need to re-read codec
parameters from the format stream. Simply continuing the loop without
seeking back to the reset point may cause the next few packets to produce
garbage samples that corrupt the Chromaprint fingerprint.

Check: does the decode loop handle `ResetRequired` by re-seeking to a
safe position, or does it simply call `decoder.reset()` and `continue`?

The safest approach for a fingerprinter (not a player) is to treat
`ResetRequired` as end-of-usable-stream and break out of the decode loop.
We already have 120 seconds of PCM at that point; a more than sufficient
fingerprint sample.

Fix:
```rust
Err(symphonia::core::errors::Error::ResetRequired) => {
    // Treat as soft end-of-stream for fingerprinting purposes.
    // We have sufficient PCM; do not risk corrupting the fingerprint.
    break 'decode;
}
```

Severity: **Medium** — rare in practice; only affects files with internal
stream resets (some VBR MP3s, certain Ogg containers).

---

**C2. lofty + Symphonia double file open under one SMB permit**

`tag_reader.rs` opens the file twice: once for lofty, once for Symphonia.
Both opens happen inside `spawn_blocking` while holding the SMB permit,
which is correct. However, the permit documentation in Pass 2.1 describes
this as a "single-pass" read, which is misleading.

This is not a bug — both reads are sequential, the permit covers both,
and there is no SMB interleaving. The fix is documentation only:

Update the comment in `tag_reader.rs`:

```rust
// Two sequential file opens under one SMB permit:
// 1. lofty::read_from_path — reads tag headers
// 2. File::open → Symphonia decode — reads audio frames
// Both are covered by the SMB permit held by the caller.
// This is intentional: lofty does not expose its file handle
// for reuse, so two opens are required.
```

Severity: **Low** — documentation only; no code change.

---

**C3. AcoustID no-op consumer channel back-pressure**

The no-op AcoustID consumer in `apps/bot/main.rs` receives
`AcoustIdRequest` messages and logs them. This consumer will be replaced
in Pass 3. However, if the no-op consumer is slower than the Orchestrator
emits (unlikely, but possible under burst conditions), the channel fills
and `acoustid_tx.send().await` in the Orchestrator blocks.

This is acceptable behavior — back-pressure is the correct response. No
fix needed. Document this as intended in a code comment:

```rust
// Bounded channel: back-pressure intentional.
// If consumer is slow, Orchestrator blocks until space is available.
// Pass 3 replaces the no-op consumer with the real AcoustID adapter.
let (acoustid_tx, mut acoustid_rx) = mpsc::channel::<AcoustIdRequest>(64);
```

Severity: **Low** — no code change; comment only.

---

**C4. EnrichmentOrchestrator first-tick skip rationale**

The Orchestrator calls `interval.tick().await` once before the main loop
to skip the first immediate tick. This delays the first DB poll by
`SCAN_INTERVAL_SECS` (default 300s) after startup.

For a fresh install with no existing tracks, this is harmless — the
reactive path handles all new tracks immediately via the scan channel.
For a restart with many `pending` tracks (e.g. after a crash mid-enrichment),
the 5-minute delay means enrichment does not resume for 5 minutes after
restart.

Recommended fix: replace the first-tick skip with an explicit initial
poll at startup. Run `claim_for_enrichment` once immediately after
spawning, before entering the `select!` loop:

```rust
// Immediate initial poll on startup — handles tracks left pending
// from a previous run without waiting for the first interval tick.
self.poll_and_emit(&acoustid_tx).await;

let mut interval = tokio::time::interval(
    Duration::from_secs(self.scan_interval_secs)
);
interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
```

Extract the DB poll logic into a private method `poll_and_emit` to avoid
duplicating the claim + emit loop.

Severity: **Medium** — affects enrichment resume time after restart.

---

#### SECTION D — Performance

**D1. Batch Classifier DB lookups (follow-up from B5)**

If the batch `find_many_by_blob_location` method was added to resolve B5,
update the Classifier task to use it.

Batch receive pattern using `tokio::sync::mpsc::Receiver::recv_many()`:

```rust
let mut batch: Vec<FileEvent> = Vec::with_capacity(64);

loop {
    // Block until at least one event arrives
    batch.clear();
    let n = file_rx.recv_many(&mut batch, 64).await;
    if n == 0 {
        break; // channel closed
    }

    // Separate removes and create/modify events
    let removes: Vec<_> = batch.iter()
        .filter(|e| e.kind == FileEventKind::Remove)
        .collect();
    let creates: Vec<_> = batch.iter()
        .filter(|e| e.kind == FileEventKind::CreateOrModify)
        .collect();

    // Handle removes individually (mark_file_missing is already cheap)
    for e in &removes {
        let rel = to_relative(&config.media_root, &e.path);
        if let Err(err) = track_repo.mark_file_missing(&rel).await {
            warn!("classifier: mark_file_missing({rel}): {err}");
        }
    }

    // Filter creates by supported extension
    let supported_creates: Vec<_> = creates.iter()
        .filter(|e| is_supported_extension(&e.path))
        .collect();

    // Batch stat() calls (still individual, but no DB round trips yet)
    let stat_results: Vec<(&FileEvent, SystemTime, u64)> = supported_creates
        .iter()
        .filter_map(|e| {
            let meta = std::fs::metadata(&e.path).ok()?;
            let mtime = meta.modified().ok()?;
            Some((*e, mtime, meta.len()))
        })
        .collect();

    // Single batch DB lookup for all paths in this batch
    let rels: Vec<String> = stat_results
        .iter()
        .map(|(e, _, _)| to_relative(&config.media_root, &e.path))
        .collect();

    let existing_map = track_repo
        .find_many_by_blob_location(&rels)
        .await
        .unwrap_or_default();

    // Process results
    for (event, mtime, size_bytes) in stat_results {
        let rel = to_relative(&config.media_root, &event.path);
        let existing = existing_map.get(&rel);

        if let Some(track) = existing {
            if let Some(db_mtime) = track.file_modified_at {
                let same = mtime_within_tolerance(
                    mtime, SystemTime::from(db_mtime), MTIME_TOLERANCE,
                ) && track.file_size_bytes == Some(size_bytes as i64);
                if same {
                    debug!("classifier: unchanged {rel}");
                    continue;
                }
            }
        }

        let _ = fp_tx.send(ToFingerprint {
            path:        event.path.clone(),
            rel:         rel.clone(),          // pre-computed, passed through
            mtime,
            size_bytes,
            existing_id: existing.map(|t| t.id),
        }).await;
    }
}
```

Note: `recv_many` is available in tokio ≥ 1.35. Verify the workspace
tokio version satisfies this before using it.

---

**D2. Fingerprint Worker — fp_concurrency permit scope**

Currently `_fp_permit` is held for the full duration of the spawned task
(SMB wait + spawn_blocking + DB write + channel emit). This means
`FINGERPRINT_CONCURRENCY` limits the entire pipeline width, including DB
write time.

For a tighter concurrency model, release `_fp_permit` immediately after
`spawn_blocking` returns (before the DB write). This allows a new file to
begin its SMB read while the previous file's DB write is still completing:

```rust
let decode_result = tokio::task::spawn_blocking(move || {
    let _permit = smb_permit;
    read_file(&path)
}).await;

drop(_fp_permit); // release fp_concurrency slot immediately after decode

// DB write and channel emit happen with no active fp_concurrency permit
match decode_result { ... }
```

This increases DB write concurrency above `FINGERPRINT_CONCURRENCY`, which
is acceptable because DB writes are not the bottleneck — SMB reads are.

Severity: **Low** — micro-optimization; only meaningful during large
batch scans. Apply only if `FINGERPRINT_CONCURRENCY` bottleneck is observed.

---

**D3. TrackRepository insert — returning the inserted row**

Check: does `TrackRepositoryImpl::insert` use `RETURNING *` or does it
execute the INSERT and then run a second `SELECT` to return the row?
The second-SELECT pattern doubles DB round trips for every new track insert.

Fix: use `sqlx::query_as::<_, Track>("INSERT INTO tracks (...) VALUES (...) RETURNING *")`
to return the inserted row in a single round trip.

Severity: **Medium** — doubles DB latency per new file index for no benefit.

---

#### SECTION E — Configuration & Dependency Audit

**E1. Verify all workspace dependencies are declared**

Run `cargo build --workspace` from a clean state and check for any missing
dependency errors. Specifically verify:

- [ ] `tokio-util` in workspace `Cargo.toml` AND in `adapters-media-store/Cargo.toml`
- [ ] `thiserror` in workspace `Cargo.toml` AND in `adapters-watcher/Cargo.toml`
- [ ] `bytes` in workspace `Cargo.toml` AND in `adapters-media-store/Cargo.toml`
- [ ] `notify = { version = "6", default-features = false }` — confirm no
      platform watcher features are accidentally enabled anywhere

**E2. Confirm no blake3 dependency anywhere**

```bash
grep -r "blake3" --include="Cargo.toml" .
```

Expected output: empty. If any crate declares `blake3`, remove it.
This is Invariant 6 from the master document.

**E3. Confirm no RecommendedWatcher usage anywhere**

```bash
grep -r "RecommendedWatcher" --include="*.rs" .
```

Expected output: empty. If found, this is Invariant 1 violation.

**E4. Confirm blob_location is never stored as absolute path**

```bash
grep -r "media_root" --include="*.rs" crates/adapters-persistence/
```

Expected output: empty — adapters-persistence should have no knowledge
of `media_root`. All `blob_location` values arriving at the persistence
layer must already be relative strings. If `media_root` appears in
persistence code, the conversion is happening in the wrong layer.

---

### Findings Report Format

Produce the following at the end of this pass:

```
## Pass 2.2 Findings Report

### Critical (must fix before Pass 3)
| ID  | File | Finding | Fixed? |
|-----|------|---------|--------|
| A1  | ...  | ...     | Yes/No |

### High
| ID  | File | Finding | Fixed? |

### Medium
| ID  | File | Finding | Deferred reason or Fixed? |

### Low
| ID  | File | Finding | Accepted / Fixed |

### Dependency Audit
| Check | Result |
|-------|--------|
| blake3 present | Not found / Found in: ... |
| RecommendedWatcher | Not found / Found in: ... |
| blob_location in persistence | Not found / Found in: ... |
| tokio-util declared | Yes / Missing in: ... |

### Net Diff Summary
Total files changed: N
Lines added: N
Lines removed: N
```

---

### Constraints

- Do not add new pipeline stages or features.
- Do not change the database schema.
- Do not change the channel capacities defined in Pass 2.1.
- Do not change `SMB_READ_CONCURRENCY`, `FINGERPRINT_CONCURRENCY`, or
  `SCAN_INTERVAL_SECS` default values.
- Any fix that requires a schema migration must be deferred to a
  dedicated migration pass and documented in the report.
- All changes must leave `cargo build --workspace` and
  `cargo test --workspace` passing.

---

### REFERENCE

*[Attach full `teamti_v2_master.md` here before sending to agent.]*
