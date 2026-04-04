TeamTI v2 — Pass 4.1 Prompt
Reflection, Correctness Audit & Performance Review (Tag Writeback)

    Review-only pass. No new features. Every change must cite a finding below.
    Read the full Pass 4 implementation before starting the checklist.
    Apply Critical and High fixes directly. Document Medium/Low if structurally out of scope.

Objective

Pass 4 introduced atomic tag writeback, a startup poller for the Pass 3 backlog, a new migration, and a fan-out path from the Cover Art Worker. This pass audits three categories of risk unique to file-system-facing code: atomicity under failure, architectural redundancy (the worker vs. port double-acquisition issue), and throughput under large backlogs.
File Inventory

text
crates/adapters-media-store/src/tag_writer.rs
crates/adapters-media-store/src/tag_writer_port.rs
crates/application/src/tag_writer_worker.rs
crates/application/src/cover_art_worker.rs
crates/application/src/events.rs
crates/adapters-persistence/src/track_repository.rs
crates/adapters-persistence/src/album_repository.rs
crates/adapters-persistence/migrations/20250006000000_tags_written_at.sql
apps/bot/src/main.rs

Run before starting:

bash
grep -rn "TODO(pass4)" --include="*.rs" .

Expected: empty. Any remaining TODO(pass4) is a missed wiring point.
Audit Checklist
SECTION A — Critical: Correctness & Safety

A1. SMB permit double-acquisition — worker vs. port

TagWriterWorker::process() acquires smb_semaphore.clone().acquire_owned() itself, then calls spawn_blocking(|| write_tags_atomic(...)) where the permit is moved into the closure.

FileTagWriterAdapter::write_tags() ALSO acquires smb_semaphore.clone().acquire_owned() before calling spawn_blocking.

Check: does TagWriterWorker::process() call write_tags_atomic directly (bypassing the port), or does it call self.tag_writer.write_tags(...) (going through the port)?

There are two bugs depending on which path is active:

    If the worker calls the port: the port acquires the permit, and the worker's earlier acquire_owned() also holds a permit simultaneously. Two permits consumed for one file operation — the effective SMB concurrency is halved silently.

    If the worker calls write_tags_atomic directly: the tag_writer: Arc<dyn FileTagWriterPort> field on the struct is dead code and the port is never exercised.

Fix: choose one pattern and remove the other.

Recommended pattern — worker delegates to port entirely:

rust
async fn process(&self, msg: ToTagWriter) -> Result<(), AppError> {
    let track = ...; // DB fetch
    let album  = ...; // DB fetch

    let tags = TagData { ... };

    // Port handles SMB semaphore acquisition and spawn_blocking internally.
    let result = self.tag_writer
        .write_tags(&msg.blob_location, &tags)
        .await?;

    self.track_repo
        .update_file_tags_written(msg.track_id, result.new_mtime, result.new_size_bytes)
        .await?;
    Ok(())
}

Remove the smb_semaphore field from TagWriterWorker. The port owns it.

Severity: Critical — double permit acquisition silently reduces effective
SMB concurrency below the configured limit.

A2. TempGuard binding — named vs. anonymous

This is the same class of bug as ScanGuard in Pass 2.2/B1. The TempGuard
must be a named let binding, not an anonymous discard.

Check in tag_writer.rs:

rust
// WRONG — drops immediately, temp file removed before write completes
let _ = TempGuard::new(&temp_path);

// CORRECT — lives to end of scope
let _temp_guard = TempGuard::new(&temp_path);

Verify that _temp_guard is used as a named binding in all paths, including
the early-return error paths inside write_tags_atomic. The guard must remain
alive until after std::fs::rename either succeeds (at which point disarm()
is called) or fails (at which point the guard drops and removes the temp file).

Additionally verify the disarm() call is positioned after rename returns
Ok, not before:

rust
std::fs::rename(&temp_path, path)?;  // ← must succeed first
_temp_guard.disarm();                 // ← only then disarm

If disarm() is called before rename and rename fails, the temp file is
left orphaned with no cleanup.

Severity: Critical — temp files accumulate on NAS if guard is misused;
original file may be unreachable if rename is attempted after guard drops.

A3. update_enriched_metadata — verify tags_written_at = NULL is present

The Pass 4 prompt requires adding tags_written_at = NULL to the existing
update_enriched_metadata SQL so re-enrichment triggers a fresh tag writeback.

Check: does the UPDATE tracks SET ... statement in
TrackRepositoryImpl::update_enriched_metadata include tags_written_at = NULL?

If missing, re-enrichment (e.g. via /rescan --force) will update DB metadata
but never re-write the file tags — the file will permanently lag the DB.

Also verify the sqlx::query! macro still compiles after the column addition.
If tags_written_at was added via migration but sqlx's offline cache is
stale, the macro may fail with a schema mismatch.

Fix if missing: add tags_written_at = NULL, to the UPDATE.
Then run: cargo sqlx prepare to refresh the offline query cache.

Severity: Critical — silent tag drift on re-enriched tracks.

A4. update_file_tags_written missing safety guard on enrichment_status

The SQL in update_file_tags_written is:

sql
UPDATE tracks SET file_modified_at = $2, file_size_bytes = $3,
                  tags_written_at = now(), updated_at = now()
WHERE id = $1

There is no guard on enrichment_status. If a non-done track ID somehow
reaches the Tag Writer channel (e.g. via a startup poller bug or a stale
channel message), its tags_written_at is set to now(), permanently
suppressing future tag writeback attempts even after the track reaches done.

Fix:

sql
UPDATE tracks SET file_modified_at = $2, file_size_bytes = $3,
                  tags_written_at = now(), updated_at = now()
WHERE id = $1
  AND enrichment_status = 'done'   -- safety guard

If the WHERE matches zero rows, the update is a no-op. No error needed —
log at DEBUG level if rows_affected() == 0.

Severity: Critical — permanent suppression of tag writeback for
prematurely queued tracks.
SECTION B — High: Correctness

B1. CoverArtWorker::tag_writer_tx is Option — silent no-op in tests

tag_writer_tx: Option<mpsc::Sender<ToTagWriter>> means that if the field is
None, the Cover Art Worker silently skips tag writeback with no log warning.
In production this is intentional (wired to Some in apps/bot/main.rs). In
integration tests that construct CoverArtWorker directly without a Tag Writer,
it is silently disabled.

Check: does apps/bot/main.rs construct CoverArtWorker with
tag_writer_tx: Some(tag_writer_tx)? Verify the None case cannot occur in
production by adding an assertion or converting the field to non-optional now
that Pass 4 is complete:

rust
// Since Pass 4, tag_writer_tx is always wired. Remove Option.
pub struct CoverArtWorker {
    // ...
    pub tag_writer_tx: mpsc::Sender<ToTagWriter>,  // ← remove Option
}

If any test constructs CoverArtWorker without a real tag writer channel,
use a dropped receiver (let (tx, _) = mpsc::channel(1)) rather than None
so the send succeeds silently without a real worker consuming it.

Severity: High — silent tag writeback bypass in production if wiring
is incomplete.

B2. Startup poller batch size vs. throughput — initial deployment backlog

find_tags_unwritten(200) returns at most 200 tracks per poll cycle. At the
default poll interval of scan_interval_secs * 4 = 1200s (20 minutes), a
50,000-track library with tags_written_at IS NULL would take:

50,000 / 200 = 250 cycles × 20 minutes = 83 hours

This means the first full tag synchronization after deploying Pass 4 takes
approximately 3.5 days.

Fix: use a tighter inner loop for the initial deployment burst. On the first
tick (startup), batch size should be much larger, or the poller should drain
the backlog continuously without waiting for the interval:

rust
// On startup, drain the entire backlog before entering the interval loop
loop {
    let batch = track_repo.find_tags_unwritten(500).await.unwrap_or_default();
    if batch.is_empty() { break; }
    for track in batch {
        let _ = tag_writer_tx.send(ToTagWriter { ... }).await;
    }
    // Small yield to avoid starving other tasks
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// Then enter normal interval-based polling
let mut interval = tokio::time::interval(Duration::from_secs(poll_interval_secs));
loop {
    interval.tick().await;
    // ... normal poll ...
}

Severity: High — days-long delay for initial tag synchronization is
unacceptable for users who deployed Pass 3 before Pass 4.

B3. AlbumRepository::find_by_id — verify method exists

TagWriterWorker::process calls self.album_repo.find_by_id(album_id). Verify:

    find_by_id is declared in the AlbumRepository trait in application/src/ports/repository.rs

    AlbumRepositoryImpl implements it

    It returns Result<Option<Album>, AppError>, not Result<Album, AppError>
    (the Option variant handles tracks with no album correctly)

If find_by_id is missing, the code will not compile. If it returns
Result<Album> (no Option), tracks without albums will panic instead of
skipping the album field gracefully.

Severity: High — compile failure or panic on Unsorted tracks.

B4. lofty::set_year — i32 to u32 cast for negative years

tags.year is Option<i32>. The writeback uses tag.set_year(year as u32).
A year value of -1 (which could appear from malformed MusicBrainz data)
casts to 4294967295 — a nonsensical year written to the file.

Fix:

rust
if let Some(year) = tags.year {
    if year > 0 {
        tag.set_year(year as u32);
    }
    // negative or zero year: skip — do not write garbage to the file
}

Severity: High — corrupted year tag in audio file for edge-case year values.

B5. Partial index idx_tracks_tags_unwritten — verify planner uses it

The migration adds:

sql
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_tracks_tags_unwritten
    ON tracks (id)
    WHERE enrichment_status = 'done' AND tags_written_at IS NULL;

The find_tags_unwritten query is:

sql
SELECT * FROM tracks
WHERE enrichment_status = 'done' AND tags_written_at IS NULL
ORDER BY updated_at ASC LIMIT $1

For a partial index on (id) to be used for this query, PostgreSQL must
see that the index WHERE clause covers the query WHERE clause. However, the
query also ORDER BY updated_at ASC — the index on (id) does not help
with this ordering. PostgreSQL may perform a full sequential scan over the
partial index result anyway.

Fix: change the partial index to include updated_at for sort efficiency:

sql
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_tracks_tags_unwritten
    ON tracks (updated_at ASC)
    WHERE enrichment_status = 'done' AND tags_written_at IS NULL;

This allows the planner to use an index scan for both the WHERE filter and
the ORDER BY in a single index traversal.

Verify with EXPLAIN (ANALYZE, BUFFERS) on the query after the index exists.

Severity: High — full sequential table scan for the poller query on
large libraries; noticeable latency spike on startup.
SECTION C — Medium: Robustness

C1. Tag Writer concurrency — unbounded task spawning

TagWriterWorker::run spawns tokio::spawn for every received message with
no bound on how many tasks can exist simultaneously. With 128-capacity channel
and a slow NAS, up to 128 tasks can be alive at once, all waiting on the
SMB semaphore. Each task holds a Tokio task stack allocation (~8 KB default).
128 tasks = ~1 MB overhead minimum; more importantly, the Tokio scheduler has
more work items than necessary.

Fix: add a TAG_WRITE_CONCURRENCY semaphore (default: 4, same as
FINGERPRINT_CONCURRENCY) to the TagWriterWorker and acquire it before
spawning:

rust
while let Some(msg) = rx.recv().await {
    let permit = self.task_semaphore.clone().acquire_owned().await
        .expect("task semaphore closed");
    let worker = Arc::clone(&self);
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(e) = worker.process(msg).await {
            warn!("tag_writer: {e}");
        }
    });
}

Add task_semaphore: Arc<Semaphore> to TagWriterWorker, initialized with
Semaphore::new(config.tag_write_concurrency) (default: 4 in Config).

Severity: Medium — task accumulation under load; Low priority if NAS is
fast, but important if SMB latency is high.

C2. lofty format support — WAV and AIFF tag writeback may silently succeed with no effect

lofty 0.21's WAV/AIFF support writes tags to an INFO or ID3 chunk. Some
WAV files written by hardware recorders have non-standard chunk layouts that
lofty parses but cannot update. save_to_path returns Ok(()) even when the
tag change had no effect.

Check: after write_tags_atomic for a WAV file, re-read the file with lofty
and verify the title tag matches the written value.

For this pass: add a post-write verification only in the integration test.
Do not add it to the production path (doubles file reads per writeback).

Test assertion:

rust
// After write_tags_atomic, re-read and verify
let verify = Probe::open(path)?.read()?;
let tag = verify.primary_tag().or_else(|| verify.first_tag());
assert_eq!(tag.and_then(|t| t.title().map(std::string::ToString::to_string)), Some(expected_title));

If this fails for WAV/AIFF, log a warning in production and set
tags_written_at = now() anyway to avoid infinite retry. The tag write is
best-effort for these formats.

Severity: Medium — silent tag write failure for WAV/AIFF files;
no corruption, but file tags do not reflect DB state.

C3. std::fs::rename on SMB — EXDEV cross-device error

On some NAS configurations, the temp directory and the music directory are
on different SMB mount points or different shares. In this case,
std::fs::rename(temp, original) returns Err(EXDEV) because rename only
works within the same filesystem.

The temp file is created in the same directory as the original (same call to
dir.join(format!(".{}.{}.tmp", ...))) specifically to prevent EXDEV. However,
if dir resolves through a symlink to a different mount point, EXDEV can still
occur.

Check: does TempGuard correctly clean up the orphaned temp file on EXDEV?
It should — if rename returns Err, the guard is still armed and drops,
removing the temp.

Verify the error message distinguishes EXDEV from other rename failures:

rust
match std::fs::rename(&temp_path, path) {
    Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
        return Err(format!(
            "rename failed (EXDEV): temp {:?} and original {:?} are on different \
             filesystems. Check for symlinks in MEDIA_ROOT.", temp_path, path
        ).into());
    }
    result => result?,
}

Add libc = "0.2" to adapters-media-store/Cargo.toml if not already present,
or use e.kind() == ErrorKind::CrossesDevices (stable in Rust 1.75+).

Severity: Medium — confusing error message on misconfigured NAS mounts;
no data loss (TempGuard handles cleanup).

C4. find_tags_unwritten — no protection against re-queueing in-progress tracks

The startup poller calls find_tags_unwritten(limit) on an interval. If a tag
write takes longer than the poll interval (unlikely but possible on slow NAS),
the same track could be returned by a second poll cycle and queued again while
the first task is still writing.

The second task will:

    Acquire SMB permit (waits behind first task)

    Re-write the same tags (harmless but wasteful)

    Set tags_written_at = now() again (harmless)

This is safe but wasteful. For this pass: document it as accepted behavior.
Add a comment to run_startup_tag_poller:

rust
// NOTE: if a tag write takes longer than poll_interval_secs, the same track
// may be re-queued by the next poll cycle. This is safe (idempotent write)
// but wastes SMB bandwidth. Consider adding a `tags_write_in_progress` column
// if this is observed in production.

Severity: Low — safe idempotent behavior; no data impact.
SECTION D — Performance

D1. DB fetch in process() — two queries where one suffices

TagWriterWorker::process makes two sequential DB calls:

    track_repo.find_by_id(track_id) — fetches track

    album_repo.find_by_id(album_id) — fetches album

For the common case (track has an album), this is 2 round trips. Add a
JOIN query to TrackRepository that returns both in one call:

rust
async fn find_with_album(
    &self,
    track_id: Uuid,
) -> Result<Option<(Track, Option<Album>)>, AppError>;

sql
SELECT t.*, a.title AS album_title, a.release_year, a.mbid AS album_mbid
FROM tracks t
LEFT JOIN albums a ON t.album_id = a.id
WHERE t.id = $1

This is a straightforward left join. It eliminates one round trip per tag
write — significant during initial 50,000-track deployment burst.

Severity: Medium — 2× DB round trips per tag write; meaningful at scale.

D2. lofty::Probe::open reads full file into memory

Probe::open(&temp_path)?.read() reads the ENTIRE audio file into memory
before any tag modification. For a 200 MB FLAC file with embedded hi-res
artwork, this allocates 200 MB on the tokio thread pool. With 4 concurrent
tag writers, peak memory during a burst can reach ~800 MB.

Mitigation: the TAG_WRITE_CONCURRENCY semaphore from C1 limits this to
tag_write_concurrency × max_file_size. Ensure tag_write_concurrency
defaults to 2 (not 4) to keep peak memory usage bounded:

text
# Default: 2 concurrent tag writes
# Limits peak memory to ~2 × largest_file_size
TAG_WRITE_CONCURRENCY=2

Document this in shared-config:

rust
/// Maximum concurrent tag write operations.
/// Each operation loads the full audio file into memory.
/// Lower this if memory pressure is observed during bulk tag writeback.
pub tag_write_concurrency: usize,  // default: 2

Severity: Medium — memory spike on large FLAC libraries with embedded artwork.
SECTION E — Dependency & Config Audit

E1. Verify cargo sqlx prepare has been run after migration 0006

After adding tags_written_at to the tracks table, all sqlx::query! macros
that touch tracks must be re-validated:

bash
cargo sqlx prepare --workspace

If the offline query cache in .sqlx/ is stale, the CI build (which likely
uses SQLX_OFFLINE=true) will fail with a schema mismatch error.

Check: is .sqlx/ committed to the repository? If yes, ensure it was
regenerated after migration 0006.

E2. lofty version pinned at 0.21 across all crates

Both adapters-media-store (tag reading from Pass 2) and the new tag
writer use lofty. Verify they use the same version:

bash
cargo tree | grep lofty

Expected: one version of lofty in the dependency tree. If two versions appear,
the Probe, Tag, and TaggedFile types from each version are distinct and
cannot be used interchangeably.

E3. No std::fs::write or std::fs::File::create on original path

Scan for direct writes to audio files that bypass the atomic rename pattern:

bash
grep -rn "fs::write\|File::create\|OpenOptions.*write" \
    --include="*.rs" crates/adapters-media-store/

Expected: no matches in tag-writing code paths. All writes must go through
the copy → modify → rename sequence.
Findings Report Format

text
## Pass 4.1 Findings Report

### Critical (applied before Pass 5)
| ID  | File | Finding | Fixed? |

### High
| ID  | File | Finding | Fixed? |

### Medium
| ID  | File | Deferred reason or Fixed? |

### Low / Accepted
| ID  | Note |

### TODO(pass4) Scan
grep output: (empty = pass, any results = fail)

### Net Diff Summary
Total files changed: N
Lines added: N
Lines removed: N

Constraints

    Do not add new pipeline stages or new enrichment logic.

    Do not change enrichment_status transitions.

    Any new migration must be 20250007000000_...sql — do not edit 0006.

    All changes must leave cargo build --workspace and
    cargo test --workspace passing.

REFERENCE

[Attach full teamti_v2_master.md here before sending to agent.]