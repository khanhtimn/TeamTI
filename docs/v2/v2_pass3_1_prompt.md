# TeamTI v2 — Pass 3.1 Prompt
## Reflection, Correctness Audit & Performance Review (Enrichment Pipeline)

> Review-only pass. No new features. Every change must cite a finding below.
> Read Pass 3 implementation in full before starting. Apply Critical and High
> fixes directly. Document Medium/Low if structurally out of scope.

---

### Objective

Pass 3 introduced three new adapter crates, three application-layer workers,
four new channel stages, and six persistence methods. This pass audits that
implementation for correctness under real-world conditions: API failures,
restarts mid-enrichment, duplicate enrichment attempts, and contention between
concurrent pipeline stages. It also identifies unnecessary DB round trips,
missing conflict handling, and one critical durability gap.

---

### File Inventory

Read every one of these files before starting the checklist:

```
crates/adapters-acoustid/src/lib.rs
crates/adapters-acoustid/src/response.rs
crates/adapters-musicbrainz/src/lib.rs
crates/adapters-musicbrainz/src/response.rs
crates/adapters-cover-art/src/lib.rs
crates/application/src/acoustid_worker.rs
crates/application/src/musicbrainz_worker.rs
crates/application/src/cover_art_worker.rs
crates/application/src/events.rs
crates/adapters-persistence/src/track_repository.rs
crates/adapters-persistence/src/artist_repository.rs
crates/adapters-persistence/src/album_repository.rs
crates/adapters-persistence/migrations/   (all files)
apps/bot/src/main.rs
```

---

### Audit Checklist

---

#### SECTION A — Critical: Durability & Data Integrity

**A1. AcoustID success path does not persist acoustid_id before proceeding**

Scenario: the AcoustID worker matches a track (score ≥ threshold), emits
`ToMusicBrainz`, and the bot crashes before the MusicBrainz worker processes
the message. On restart, the track has `enrichment_status = 'enriching'` (set
by `claim_for_enrichment`), but the `acoustid_id` and `enrichment_confidence`
columns are NULL. The stale watchdog resets the row to `pending`. The next
enrichment cycle re-queries AcoustID unnecessarily, consuming an API call.

Check: on the success path in `acoustid_worker.rs`, is there a DB write that
stores `acoustid_id` and `enrichment_confidence` BEFORE emitting `ToMusicBrainz`?

If not, the fix is to add a targeted UPDATE at the acoustid success point:

```rust
// Persist AcoustID result immediately — makes it durable before the
// MusicBrainz stage begins, so a crash here doesn't lose the match.
sqlx::query!(
    r#"
    UPDATE tracks
    SET acoustid_id           = $2,
        enrichment_confidence = $3,
        updated_at            = now()
    WHERE id = $1
    "#,
    req.track_id, m.acoustid_id, m.score
)
.execute(&self.repo_pool) // or add a dedicated port method
.await?;
```

Add `update_acoustid_match(track_id, acoustid_id, confidence)` to
`TrackRepository` trait and its `TrackRepositoryImpl`. Then call it before
`mb_tx.send(...)`.

Severity: **Critical** — API credits wasted on restarts; crash-safe pipeline
requires durability at each stage boundary.

---

**A2. `upsert_track_artist` and `upsert_album_artist` missing conflict handling**

Scenario: a track is re-enriched (e.g. after a `--rescan --force`). The
MusicBrainz worker calls `upsert_track_artist` and `upsert_album_artist` for
each artist credit. If these INSERT statements have no `ON CONFLICT` clause,
they will violate the unique constraint on `(track_id, artist_id)` and
`(album_id, artist_id)` on the second enrichment run.

Check: do the SQL statements in `ArtistRepositoryImpl::upsert_track_artist`
and `upsert_album_artist` contain `ON CONFLICT ... DO NOTHING` or
`ON CONFLICT ... DO UPDATE`?

Fix if missing:
```sql
-- track_artists join
INSERT INTO track_artists (track_id, artist_id, role, position)
VALUES ($1, $2, $3, $4)
ON CONFLICT (track_id, artist_id) DO UPDATE
    SET role     = EXCLUDED.role,
        position = EXCLUDED.position

-- album_artists join
INSERT INTO album_artists (album_id, artist_id, role, position)
VALUES ($1, $2, $3, $4)
ON CONFLICT (album_id, artist_id) DO UPDATE
    SET role     = EXCLUDED.role,
        position = EXCLUDED.position
```

Severity: **Critical** — silent 500 error on every re-enrichment run; track
gets stuck in `enriching` permanently after the first enrichment.

---

**A3. `search_vector` tsvector column — verify trigger or generated column**

The `TrackSearchPort::search` implementation uses:
```sql
WHERE search_vector @@ plainto_tsquery('music_simple', $1)
```

For this to work, `search_vector` must be:
1. A `GENERATED ALWAYS AS (...) STORED` column, OR
2. A `tsvector` column populated by an `AFTER INSERT OR UPDATE` trigger

Check: does the migration that creates `tracks` include either a
`GENERATED ... STORED` expression or a trigger definition that populates
`search_vector` on both INSERT and UPDATE?

Critical issue: if the trigger exists but only fires on INSERT (not UPDATE),
then `update_enriched_metadata` — which writes the title and artist_display
after the initial scan insert — will leave `search_vector` stale (pointing
to the filename-based title, not the enriched title). The track will be
unsearchable by its real title.

Fix: verify the trigger fires on `INSERT OR UPDATE OF title, artist_display`.
If it only fires on INSERT, add `OR UPDATE` to the trigger definition.

The correct trigger:
```sql
CREATE OR REPLACE FUNCTION tracks_search_vector_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search_vector :=
        setweight(to_tsvector('music_simple', coalesce(NEW.title, '')), 'A') ||
        setweight(to_tsvector('music_simple', coalesce(NEW.artist_display, '')), 'B');
    NEW.search_text :=
        lower(NEW.title || ' ' || coalesce(NEW.artist_display, ''));
    RETURN NEW;
END;
$$;

CREATE TRIGGER tracks_search_vector_trig
BEFORE INSERT OR UPDATE OF title, artist_display
ON tracks FOR EACH ROW
EXECUTE FUNCTION tracks_search_vector_update();
```

If the migration exists and is correct, mark A3 as NOT PRESENT.
If missing or incomplete, add a new migration `20250005000000_search_trigger.sql`.

Severity: **Critical** — enriched tracks unsearchable by title in Discord.

---

#### SECTION B — High: Correctness

**B1. MusicBrainz worker makes redundant `find_by_mbid` DB call per artist**

In `musicbrainz_worker.rs`, the worker upserts each artist and receives the
upserted row back. It then calls `artist_repo.find_by_mbid(&credit.artist_mbid)`
a second time to get the artist's UUID for the `AlbumArtist` join. This is an
unnecessary extra DB round trip per artist credit.

Check: is there a second DB call to look up an artist that was just upserted?

Fix: collect upserted artists in a local HashMap during the artist loop,
then reuse them for album_artist without an extra query:

```rust
let mut upserted_artists: Vec<Artist> = Vec::new();

for (i, credit) in recording.artist_credits.iter().enumerate() {
    // ... build artist ...
    let upserted = self.artist_repo.upsert(&artist).await?;
    let _ = self.artist_repo.upsert_track_artist(&TrackArtist {
        track_id:  msg.track_id,
        artist_id: upserted.id,
        role,
        position: (i + 1) as i32,
    }).await;
    upserted_artists.push(upserted);
}

// Reuse upserted_artists for AlbumArtist — no second DB call
for (i, artist) in upserted_artists.iter().enumerate() {
    let _ = self.artist_repo.upsert_album_artist(&AlbumArtist {
        album_id:  upserted_album.id,
        artist_id: artist.id,
        role:      if i == 0 { ArtistRole::Primary } else { ArtistRole::Featuring },
        position:  (i + 1) as i32,
    }).await;
}
```

Severity: **High** — doubles DB calls per artist credit; significant for
collaborative tracks and re-enrichment runs.

---

**B2. `update_enrichment_status` called on AcoustID success is a no-op**

On the success path, `acoustid_worker.rs` calls:
```rust
self.repo.update_enrichment_status(
    req.track_id,
    &EnrichmentStatus::Enriching,  // ← already 'enriching' from claim_for_enrichment
    track.enrichment_attempts,      // ← unchanged
    None,
).await;
```

The track is already `enriching` (set by `claim_for_enrichment`). Setting it
to `Enriching` again with the same attempt count is a wasted DB write on every
successful AcoustID match. After A1 is fixed, this call becomes the only place
that writes to the track on success — and it writes nothing new.

Fix: remove this `update_enrichment_status` call entirely from the success
branch. The acoustid_id/confidence write from A1's fix is sufficient at this
stage. Status transitions to `done` in the Cover Art Worker.

Severity: **High** — wasted DB round trip per successful enrichment; misleading
code that suggests a meaningful state transition is happening.

---

**B3. AcoustID score selection ignores results with multiple recordings**

The current code takes `r.recordings[0]` from the best-scored result. However,
AcoustID sometimes returns a result where the first recording is a live version
or an alternate take, while the canonical studio recording appears later in the
list. The raw array order from AcoustID is not ranked by recording quality.

Check: does the adapter take `recordings[0]` unconditionally, or does it
select the recording that best matches the requested duration?

Recommended fix: among the recordings in the best-scored result, prefer the
one whose `duration` is closest to `fp.duration_secs`:

```rust
// AcoustID recording duration (if available) vs. our decoded duration
let best_recording = r.recordings
    .iter()
    .min_by_key(|rec| {
        rec.duration
            .map(|d| (d as i64 - fp.duration_secs as i64).unsigned_abs())
            .unwrap_or(u64::MAX)
    });
```

Add an optional `duration: Option<u32>` field to `AcoustIdRecording` in
`response.rs`:
```rust
#[derive(Debug, Deserialize)]
pub struct AcoustIdRecording {
    pub id:       String,
    pub duration: Option<u32>,
}
```

Severity: **High** — wrong MBID selected for tracks where duration diverges
between live and studio versions; causes enrichment with wrong metadata.

---

**B4. MusicBrainz release selection — first is not always most relevant**

`body.releases.into_iter().next()` takes the first release from the MusicBrainz
API response. The API does not guarantee ordering. A recording may appear on
an original studio album, a greatest hits compilation, a remaster, and a live
album — and the compilation may appear first.

Check: is any release selection priority applied, or is it purely positional?

Fix: prefer releases in this priority order:
1. Release type is `Album` (studio album) and release status is `Official`
2. Any `Official` release
3. Any release with a release year
4. First available

To do this, add `release-groups` to the MusicBrainz `inc` parameter:
```
?inc=releases+artists+genres+release-groups
```

Then add to `MbRelease`:
```rust
#[serde(default)]
#[serde(rename = "release-group")]
pub release_group: Option<MbReleaseGroup>,

#[serde(default)]
pub status: Option<String>,  // "Official", "Bootleg", "Promotion", etc.
```

```rust
#[derive(Debug, Deserialize)]
pub struct MbReleaseGroup {
    #[serde(rename = "primary-type")]
    pub primary_type: Option<String>, // "Album", "Single", "EP", "Compilation", etc.
}
```

Selection priority function:
```rust
fn release_priority(r: &MbRelease) -> u8 {
    let is_official = r.status.as_deref() == Some("Official");
    let is_album    = r.release_group.as_ref()
        .and_then(|rg| rg.primary_type.as_deref()) == Some("Album");
    match (is_album, is_official) {
        (true,  true)  => 0,  // best: official studio album
        (false, true)  => 1,  // official, not album
        (true,  false) => 2,  // unofficial album
        (false, false) => 3,  // other
    }
}

let release = body.releases
    .into_iter()
    .min_by_key(|r| release_priority(r));
```

Severity: **High** — tracks enriched from compilations get wrong album
metadata, cover art, and release year.

---

**B5. `AppError::NotFound` variant — verify existence**

The MusicBrainz adapter returns `Err(AppError::NotFound(...))` for 404
responses. Verify `NotFound` is a defined variant of `AppError` in
`crates/application/src/error.rs`.

If the variant does not exist, the adapter will not compile. If it was
added in Pass 1 but not exported from `application/src/lib.rs`, it will
be invisible to callers.

Check: does `AppError` have a `NotFound(String)` variant that is `pub`?

If missing, add it:
```rust
#[error("not found: {0}")]
NotFound(String),
```

Severity: **High** — compile failure if missing.

---

**B6. Cover Art Worker processes messages sequentially — semaphore wasted**

The Cover Art Worker uses a single `while let` loop, processing one
`ToCoverArt` message at a time. The `COVER_ART_CONCURRENCY` semaphore in
`CoverArtAdapter` is never actually exercised because only one request is
ever in flight at a time.

Check: does `cover_art_worker.rs` spawn a task per message, or does it
await each message sequentially?

Fix: spawn a task per message and let the adapter's semaphore control
actual concurrency:

```rust
pub async fn run(self: Arc<Self>, mut rx: mpsc::Receiver<ToCoverArt>) {
    while let Some(msg) = rx.recv().await {
        let worker = Arc::clone(&self);
        // Semaphore inside CoverArtAdapter gates actual HTTP concurrency.
        tokio::spawn(async move {
            worker.process(msg).await;
        });
    }
}
```

Without this change, cover art fetching is serialized at 1 req per
MusicBrainz-worker cycle — effectively 1 cover art per second maximum,
regardless of `COVER_ART_CONCURRENCY` setting.

Severity: **High** — cover art throughput artificially bottlenecked;
large libraries take proportionally longer to reach `done`.

---

#### SECTION C — Medium: Robustness

**C1. Cover Art extraction — no SMB semaphore**

`CoverArtAdapter::extract_from_tags` opens the audio file via `lofty` inside
`spawn_blocking` without acquiring `SMB_READ_SEMAPHORE`. This can cause SMB
bandwidth contention with the Fingerprint Worker during large scan+enrichment
overlaps (e.g. initial library indexing).

The question is whether to enforce the semaphore here. There are two positions:
- **Enforce it:** cover art extraction is a file read and should respect the
  NAS bandwidth budget. Add a `smb_semaphore: Arc<Semaphore>` field to
  `CoverArtAdapter` and acquire it before `spawn_blocking`.
- **Exclude it:** cover art extraction is best-effort and happens during the
  enrichment phase (after fingerprinting). Fingerprint Worker and Cover Art
  Worker rarely overlap at scale. The lofty read is also much shorter than
  a full decode — typically only the first few KB of the file.

Decision for this pass: **exclude from SMB semaphore** but document the
rationale explicitly with a comment. If NAS contention is observed in
production, a follow-up can add it. The comment must reference the master
document's SMB invariant.

Add to `extract_from_tags`:
```rust
// SMB_READ_SEMAPHORE is intentionally NOT acquired here.
// Rationale: cover art extraction reads only the tag header (typically
// < 256 KB, often cached), not the full audio stream. The fingerprint
// decode (which reads up to 120s of PCM) dominates SMB bandwidth.
// The two workers (Fingerprint and Cover Art) rarely overlap at scale.
// If NAS contention is observed, promote this to a semaphore-guarded read.
// See: master doc §Invariant 4 and §adapters-cover-art.
```

Severity: **Medium** — not a correctness issue; affects NAS performance
under specific timing conditions.

---

**C2. MusicBrainz User-Agent format not validated at startup**

MusicBrainz requires User-Agent in the format:
`AppName/1.0 (https://contact-url.example.com)`.
Missing or malformed User-Agent causes throttling or IP bans.

Check: is `config.mb_user_agent` validated at startup before the adapter
is constructed? Or is it passed through directly to reqwest?

Fix: add a startup validation in `apps/bot/main.rs` (or in the adapter
constructor) that verifies the User-Agent contains `/` and `(`:

```rust
impl MusicBrainzAdapter {
    pub fn new(user_agent: String) -> Self {
        assert!(
            user_agent.contains('/') && user_agent.contains('('),
            "MB_USER_AGENT must be in format 'AppName/version (contact-url)'. \
             Got: {user_agent:?}"
        );
        // ...
    }
}
```

Use `assert!` (panic at startup) not a log warning. A missing user agent
causes silent, delayed failures (IP ban surfacing hours later), not
immediate errors.

Severity: **Medium** — configuration error surfaces hours after deployment
as an IP ban rather than a startup panic.

---

**C3. `lofty` picture type selection — `PictureType::Other` is too broad**

The cover art extraction in `adapters-cover-art` selects:
```rust
.find(|p| {
    p.pic_type() == lofty::picture::PictureType::CoverFront
    || p.pic_type() == lofty::picture::PictureType::Other
})
```

`PictureType::Other` includes artist photos, back covers, band logos, and
lyric sheets — not just cover art. For tracks tagged by tools that don't
set `CoverFront` specifically, this fallback can save wrong images to
`cover.jpg`.

Fix: expand the priority list instead of falling back to `Other`:
```rust
use lofty::picture::PictureType::*;
const PREFERRED: &[lofty::picture::PictureType] = &[
    CoverFront, Media, LeafletPage, Illustration
];

// Try preferred types in order
let art = PREFERRED.iter().find_map(|ptype| {
    t.pictures().iter().find(|p| p.pic_type() == *ptype)
});
// Do NOT fall back to Other — it includes logos and artist photos
```

Severity: **Medium** — wrong image written to `cover.jpg` for some files;
not data-corrupt but user-visible.

---

**C4. `reqwest` version must be consistent across all adapter crates**

Three adapter crates declare `reqwest = "0.12"`. If any crate resolves to
a different minor version due to semver flexibility and feature differences,
Cargo may compile two versions of reqwest, increasing binary size and
potentially causing `hyper` conflicts.

Check:
```bash
cargo tree -d 2>&1 | grep reqwest
```

Expected: all three crates resolve to the same reqwest version.

Fix: pin to exact minor version in workspace:
```toml
# workspace Cargo.toml
[workspace.dependencies]
reqwest = { version = "=0.12.X", default-features = false,
            features = ["json", "rustls-tls"] }
```

Replace `X` with the actual resolved version from `cargo tree`.

Severity: **Medium** — binary bloat; potential link-time conflicts with
multiple hyper versions.

---

**C5. governor `quanta` feature — TSC availability on VM hosts**

The governor crate is configured with `features = ["std", "quanta"]`.
The `quanta` feature uses the CPU TSC (Time Stamp Counter) for high-precision
rate limit timing. On some cloud VMs (KVM with TSC migration disabled,
certain ARM instances), the TSC is unreliable or virtualized, causing
governor's limiter to drift.

This is a deployment concern, not a code bug. Add an env-var override to
disable `quanta` timing in containerized environments:

The safest fix is to remove `quanta` from features — the default `std`
timer is sufficient for 1 req/sec rate limiting:
```toml
governor = { version = "0.6", default-features = false, features = ["std"] }
```

The `quanta` feature matters for sub-millisecond rate limits. At 1 req/sec,
the precision difference is irrelevant.

Severity: **Low** — potential issue on specific deployment targets only.

---

#### SECTION D — Performance

**D1. AcoustID worker fetches the full track row just to read `enrichment_attempts`**

On every received `AcoustIdRequest`, the worker does:
```rust
let track = self.repo.find_by_id(req.track_id).await.ok().flatten();
let attempts = track.map(|t| t.enrichment_attempts + 1).unwrap_or(1);
```

This full `SELECT *` is only to read `enrichment_attempts`. For high-volume
enrichment bursts, this doubles the DB load of the AcoustID worker.

Fix: add a targeted `find_attempts` method to `TrackRepository`:
```rust
async fn get_enrichment_attempts(&self, id: Uuid) -> Result<i32, AppError>;
```

```sql
SELECT enrichment_attempts FROM tracks WHERE id = $1
```

This reads one integer column instead of the entire row.

Better fix (eliminates the query entirely): pass `enrichment_attempts` through
the `AcoustIdRequest` message from the Enrichment Orchestrator, which already
has the full track row from `claim_for_enrichment`:

```rust
pub struct AcoustIdRequest {
    pub track_id:           Uuid,
    pub fingerprint:        String,
    pub duration_secs:      u32,
    pub enrichment_attempts: i32,  // ← carry through from claim_for_enrichment
}
```

The Orchestrator already has `track.enrichment_attempts` in scope when it
emits `AcoustIdRequest`. Passing it through eliminates one DB read per
enrichment cycle.

Severity: **Medium** — extra DB round trip per enrichment; doubles at scale
(10,000 tracks = 10,000 extra reads during initial enrichment).

---

**D2. MusicBrainz worker fetches track row to derive `album_dir`**

After all the MusicBrainz upserts, the worker re-fetches the track via
`self.track_repo.find_by_id(msg.track_id)` only to read `blob_location` and
derive `album_dir`. The `blob_location` was known at the start of the
Fingerprint Worker stage and should have been carried through the pipeline.

Fix: add `blob_location: String` to `ToMusicBrainz`:
```rust
pub struct ToMusicBrainz {
    pub track_id:      Uuid,
    pub mbid:          String,
    pub acoustid_id:   String,
    pub confidence:    f32,
    pub duration_secs: u32,
    pub blob_location: String,  // ← carried from TrackScanned
}
```

The Enrichment Orchestrator's reactive path receives `TrackScanned`, which
has `blob_location` (the field was added in Pass 2.2's B2 fix). Pass it
through: `TrackScanned → AcoustIdRequest → ToMusicBrainz → ToCoverArt`.

This eliminates the `find_by_id` call in the MusicBrainz worker and the
separate DB call in the Cover Art Worker.

Severity: **Medium** — one unnecessary DB read per enrichment in the
MusicBrainz and Cover Art workers; cascade saving at scale.

---

**D3. `plainto_tsquery` vs `phraseto_tsquery` for multi-word search**

`TrackSearchPort::search` uses `plainto_tsquery` which strips stop words and
treats the query as a bag of words. For music search, phrase proximity matters:
"led zeppelin" should rank higher than documents containing "led" and "zeppelin"
far apart.

Consider `phraseto_tsquery` for exact phrase matching or `websearch_to_tsquery`
for a more flexible user-facing query syntax:

```sql
WHERE search_vector @@ websearch_to_tsquery('music_simple', $1)
```

`websearch_to_tsquery` supports `"exact phrase"`, `-exclude`, and `OR` — all
useful for music search without requiring users to know tsquery syntax.

Severity: **Low** — search quality improvement; no correctness impact.

---

#### SECTION E — Config & Dependency Audit

**E1. Verify no `reqwest::blocking` usage anywhere**

```bash
grep -r "reqwest::blocking\|Client::new()\|blocking::Client" \
    --include="*.rs" crates/adapters-acoustid/ \
    crates/adapters-musicbrainz/ crates/adapters-cover-art/
```

Expected: no results. Any blocking HTTP client violates Invariant 15 and
will deadlock inside a tokio runtime.

Also verify `reqwest::Client::builder()` is used (not `Client::new()`), since
`Client::new()` uses global defaults and does not apply the custom timeouts
or redirect policies required by this implementation.

**E2. Verify governor limiters are independent instances**

```bash
grep -n "RateLimiter::direct" \
    crates/adapters-acoustid/src/lib.rs \
    crates/adapters-musicbrainz/src/lib.rs
```

Expected: one `RateLimiter::direct` per file. If the same `Arc<Limiter>` is
somehow shared between both adapters (e.g. passed through config or app state),
AcoustID and MusicBrainz will share a 1 req/sec bucket — halving effective
throughput to 0.5 req/sec per service.

**E3. Cover Art Archive User-Agent**

While CAA does not enforce User-Agent like MusicBrainz, sending a descriptive
User-Agent is good practice. Verify the `CoverArtAdapter` HTTP client sends
a User-Agent header:

```rust
let client = Client::builder()
    .user_agent("TeamTI/2.0 (github.com/your-repo)")  // project identity
    .timeout(Duration::from_secs(20))
    .redirect(reqwest::redirect::Policy::limited(5))
    .build()?;
```

**E4. All three adapter crates — `thiserror` used but errors wrapped to AppError**

Verify that no adapter crate exposes its own `Error` enum publicly — all
errors must be converted to `AppError` at the port boundary. Internal error
types (for `?` ergonomics) are fine as `pub(crate)`.

```bash
grep -n "pub enum.*Error" \
    crates/adapters-acoustid/src/lib.rs \
    crates/adapters-musicbrainz/src/lib.rs \
    crates/adapters-cover-art/src/lib.rs
```

Expected: no `pub enum` — only `pub(crate)` or internal enums.

---

### Findings Report Format

```
## Pass 3.1 Findings Report

### Critical (applied before Pass 4)
| ID  | File | Finding | Fixed? |
|-----|------|---------|--------|
| A1  | ...  | ...     | Yes/No |

### High
| ID  | File | Finding | Fixed? |

### Medium
| ID  | File | Deferred reason or Fixed? |

### Low
| ID  | File | Accepted / Fixed |

### Message Type Changes (if D2 applied)
List any fields added to ToMusicBrainz, AcoustIdRequest, TrackScanned.
These are pipeline-contract changes — document them so Pass 4 implementations
are aware.

### Net Diff Summary
Total files changed: N
Lines added: N
Lines removed: N
```

---

### Constraints

- Do not add new pipeline stages, crates, or database tables.
- Do not change channel capacities defined in Pass 3.
- Do not implement tag writeback — that is Pass 4.
- Any migration changes (e.g. adding search trigger) must be a new migration
  file, not an edit to existing migrations.
- All changes must leave `cargo build --workspace` passing.
- If D2 (blob_location carry-through) is applied, update ALL intermediate
  message structs (`AcoustIdRequest`, `ToMusicBrainz`) and ALL their
  construction sites.

---

### REFERENCE

docs/v2/v2_master.md