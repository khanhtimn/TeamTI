# TeamTI v2.1 — Pass 1.1 Prompt
## Audit: LRCLIB Lyrics Integration

> Review-only pass. No changes applied without approval.
> Read the full LRCLIB.md implementation plan and all affected files
> before writing any findings. The agent audits freely — no findings
> are pre-enumerated.

---

### Context

v2.1 adds lyric fetching as a new enrichment stage inserted between
the MusicBrainz worker and the Cover Art worker:

```
... → MusicBrainzWorker → [NEW] LyricsWorker → CoverArtWorker → TagWriter → ...
```

The implementation plan (`LRCLIB.md`) defines:
- A new `LyricsProviderPort` trait in the application layer
- A new `LyricsWorker` in the application layer
- A new `adapters-lyrics` crate implementing the port
- The adapter performs a two-step lookup: local sidecar `.lrc` file first,
  LRCLIB HTTP API second
- Lyrics are persisted via a new `update_lyrics` repository method
- The TagWriter (Pass 7, F2) already writes `ItemKey::Lyrics` — so lyrics
  in DB will be embedded on the next tag write pass automatically

The open questions from the plan (syncedLyrics vs plainLyrics format;
sidecar `.lrc` write-back to disk) are still unresolved and must be
addressed in this pass.

---

### Acceptance Gate

Run before any investigation:

```bash
cargo clippy --workspace --all-targets 2>&1
cargo test --workspace 2>&1
cargo sqlx prepare --check --workspace 2>&1
```

If any command fails, that is a CRITICAL finding. List full output
before proceeding.

---

### Audit Checklist

---

#### SECTION A — Critical: API Correctness & Contract

**A1. LRCLIB response contract — null fields vs absent fields**

The LRCLIB `GET /api/get` endpoint returns a JSON object when a match
is found and a `404` when no match exists. Verify the implementation
handles both correctly:

1. **404 response:** The adapter must return `Ok(None)`, NOT
   `Err(AppError::...)`. A missing match is not an error — it is a
   valid soft-skip. Verify the HTTP status branch:
   ```rust
   StatusCode::NOT_FOUND => return Ok(None),
   ```
   If a 404 is propagated as an `Err`, the `LyricsWorker` will mark
   the track as `Failed` instead of continuing to `ToCoverArt`.

2. **Null `syncedLyrics` / `plainLyrics`:** Even on a 200 response,
   LRCLIB may return `"syncedLyrics": null` for tracks where only one
   format is available. Verify the response struct uses `Option<String>`
   for both fields, not `String`:
   ```rust
   pub struct LrclibResponse {
       pub synced_lyrics: Option<String>,
       pub plain_lyrics:  Option<String>,
   }
   ```
   If either is `String`, deserialization panics or errors on null.

3. **Empty string vs null:** LRCLIB sometimes returns `""` (empty string)
   instead of `null` for a missing format. Verify the fallback chain
   handles both `None` and `Some("")` as "not available":
   ```rust
   synced_lyrics.filter(|s| !s.is_empty())
       .or_else(|| plain_lyrics.filter(|s| !s.is_empty()))
   ```

4. **`duration` parameter precision:** The port signature uses
   `duration_secs: u32`. If the DB stores `duration_ms: i64`, integer
   division truncates: a 225,500ms track becomes 225s, but LRCLIB may
   have catalogued it as 226s. LRCLIB performs fuzzy duration matching
   (±2 seconds tolerance per their docs), but verify the truncation
   does not push a borderline track outside the tolerance window.
   Consider rounding (`(duration_ms + 500) / 1000`) instead of
   truncating.

Severity: **Critical** — 404 propagated as error permanently fails valid
tracks; null `String` deserialization panics the worker.

---

**A2. `album_name: Option<&str>` — query parameter serialization**

The `fetch_lyrics` signature accepts `album_name: Option<&str>`. When
`album_name` is `None`, the LRCLIB `album_name` query parameter must be
**omitted entirely** — not sent as an empty string or the literal `"None"`.

Verify how the reqwest query builder handles this. The correct pattern:

```rust
let mut params = vec![
    ("track_name", track_name),
    ("artist_name", artist_name),
];
if let Some(album) = album_name {
    params.push(("album_name", album));
}
params.push(("duration", &duration_secs.to_string()));

client.get(URL).query(&params).send().await
```

Incorrect patterns that will silently degrade match quality:
- `.query(&[..., ("album_name", album_name.unwrap_or(""))])` — sends empty string
- Using a struct with `#[serde(skip_serializing_if = "Option::is_none")]`
  without verifying reqwest respects that attribute in `.query()`

Severity: **Critical** — spurious `album_name=` parameter causes LRCLIB
to fail matching on tracks where album is unknown.

---

**A3. Rate limiting — LRCLIB is a public API with enforcement**

The implementation plan describes `LyricsWorker` as "highly-concurrent"
but mentions no rate limiting for LRCLIB requests.

LRCLIB enforces rate limits on its public API. Unthrottled concurrent
requests from a bulk enrichment run will trigger 429 responses within
minutes. Verify:

1. Is there a rate limiter (e.g. `governor` bucket) applied before each
   LRCLIB request, analogous to the existing AcoustID and MusicBrainz
   rate limiters?

2. Does the adapter handle HTTP 429 correctly?
   - Is it returned as `Err(AppError::... { retryable: true })`?
   - Does `LyricsWorker` treat a retryable error as a soft failure
     (re-queue or backoff) rather than marking the track `Failed`?

3. Is `LYRICS_CONCURRENCY` defined as a config variable in `shared-config`
   analogous to `MB_FETCH_CONCURRENCY`? If the worker is "highly-concurrent"
   without a concurrency cap, back-pressure from LRCLIB will manifest as
   a flood of 429 errors rather than a controlled slowdown.

Severity: **Critical** — unthrottled requests to LRCLIB on a large
library causes IP-level rate limiting or temporary ban, blocking all
future lyrics enrichment.

---

#### SECTION B — High: Architecture & Design

**B1. `blob_location` in the port interface — architecture boundary violation**

`LyricsProviderPort::fetch_lyrics` accepts `blob_location: &str` as its
first parameter. This leaks a filesystem concern (sidecar `.lrc` path
construction) into the application layer port interface.

The application layer should not know about filesystem paths — that is
an adapter implementation detail. Evaluate two alternatives:

**Option 1 (Recommended):** Split the port into two concerns:
```rust
// Port: purely domain-facing, no filesystem knowledge
pub trait LyricsProviderPort: Send + Sync {
    async fn fetch_lyrics(
        &self,
        track_name:   &str,
        artist_name:  &str,
        album_name:   Option<&str>,
        duration_secs: u32,
    ) -> Result<Option<LyricResult>, AppError>;
}
```
The sidecar file check becomes a filesystem port call in `LyricsWorker`
(using `MediaStorePort` or equivalent) before calling `fetch_lyrics`.

**Option 2 (Pragmatic):** Keep `blob_location` in the port but document
the architectural trade-off explicitly. This is acceptable if the
MediaStore port does not already provide a clean abstraction for sidecar
file access.

State which option the implementation chose and whether it is consistent
with the hexagonal architecture enforced in the rest of the codebase.
If Option 2 was chosen, verify it at least does not introduce a hard
dependency on `std::fs` inside the application crate.

Severity: **High** — architecture violation; acceptable if consciously
chosen and documented, blocking if `std::fs` appears in `application/`.

---

**B2. `update_lyrics` missing from `TrackRepositoryPort` trait**

The plan adds `update_lyrics` to `adapters-persistence/.../track_repository.rs`
(the concrete implementation). Verify it is also added to the
`TrackRepositoryPort` trait in `application/src/ports/repository.rs`.

If it is only on the concrete type:
- `LyricsWorker` must depend on the concrete `TrackRepositoryImpl` directly
- This breaks the port/adapter boundary — the worker can no longer be
  tested with a mock repository
- It creates a hard compile-time coupling between `application` and
  `adapters-persistence`

Run:
```bash
grep -rn "update_lyrics" --include="*.rs" \
    crates/application/src/ports/ \
    crates/adapters-persistence/src/
```
Expected: method present in both locations.

Severity: **High** — compile-time coupling violation; `LyricsWorker`
becomes untestable in isolation.

---

**B3. `ToLyrics` event — metadata fallback for unmatched tracks**

The `ToLyrics` event carries "authoritative title, artist, album,
duration_secs sourced from MB." For tracks where MusicBrainz found no
match (`enrichment_status = no_match`), these MB-sourced fields will be
`None` or absent.

Determine how `LyricsWorker` handles this case:

1. Does it fall back to file-tag metadata (title/artist from the scanned
   `Track` record)?
2. Does it skip LRCLIB and pass directly to `ToCoverArt`?
3. Does it attempt LRCLIB with `None` artist/album (which degrades match quality)?

Option 2 is the most conservative and correct: if MB could not identify
the track, LRCLIB is unlikely to match it either. Option 1 is acceptable
but risks false-positive lyric matches on ambiguous titles.

Verify which path is actually implemented and whether it is consistent
with the intent described in `LRCLIB.md`.

Severity: **High** — undefined behavior for the ~10–30% of tracks that
do not have a MusicBrainz match.

---

**B4. `LyricsWorker` skip — redundant DB read**

`LyricsWorker` step 1 is "Retrieve `Track` from DB." Step 2 is "If
`track.lyrics.is_some()` → skip."

This means every track that already has embedded lyrics (scanned from
file tags in the initial pass) incurs a DB round-trip to determine it
should be skipped. For a 50,000 track library on re-enrichment, this is
50,000 unnecessary DB reads.

Better: the MusicBrainz worker already holds the full `Track` struct
when it emits `ToLyrics`. Include `has_lyrics: bool` (or the actual
`lyrics: Option<String>`) in the `ToLyrics` event struct. Then
`LyricsWorker` skips without a DB read when `has_lyrics` is true.

Verify whether `ToLyrics` carries this field. If not, flag as
optimization.

Severity: **High** for large libraries; **Medium** for personal NAS
scale. Classify based on the project's target library size.

---

**B5. Synchronized lyrics stored as LRC format — tag write incompatibility**

When LRCLIB returns `syncedLyrics`, the content is in LRC format:
```
[00:15.12] Hello, it's me
[00:18.45] I was wondering if after all these years
```

This is stored in `tracks.lyrics` (TEXT column) and later embedded by
`tag_writer.rs` via `insert_text(ItemKey::Lyrics, ...)`.

`ItemKey::Lyrics` maps to:
- **ID3v2:** `USLT` (Unsynchronized Lyrics Text) — plain text only.
  LRC timestamps embedded in USLT will appear as literal `[00:15.12]`
  characters in media players that display lyrics.
- **FLAC/Vorbis:** `LYRICS` tag — also plain text by convention.

For synchronized lyrics in ID3v2, the correct tag is `SYLT`
(Synchronized Lyrics), which has a binary structured format that
lofty's `ItemKey::Lyrics` does NOT map to.

Evaluate three options:
1. **Store and embed as plain only:** Strip LRC timestamps before DB
   storage. Simpler, universally compatible.
2. **Store both formats separately:** `lyrics_plain: Option<String>` and
   `lyrics_synced: Option<String>` in DB. Embed plain in tags; expose
   synced for future Discord karaoke feature.
3. **Store LRC as-is, embed as-is:** Acceptable if the primary consumer
   is a Discord display (not a media player), and if media players with
   embedded LRC tags are acceptable in the library.

This decision directly answers Open Question 1 from `LRCLIB.md`.
Make a recommendation and verify the implementation matches it.

Severity: **High** — if plain-text embedding is desired but LRC is stored
as-is, every media player in the library shows raw timestamps in lyrics.

---

#### SECTION C — Medium: Robustness

**C1. Sidecar file read — blocking I/O in async context**

The sidecar detection uses `std::fs::metadata(path)` and presumably
`std::fs::read_to_string(path)` to read the `.lrc` file content. These
are blocking syscalls. Called inside an async task, they block the Tokio
thread for the duration of the I/O, reducing concurrency for the entire
worker pool.

Run:
```bash
grep -rn "std::fs::" --include="*.rs" crates/adapters-lyrics/src/
```

Any result is a finding. Replace with `tokio::fs::metadata` and
`tokio::fs::read_to_string` for non-blocking async I/O.

Severity: **Medium** — performance degradation under concurrent workers;
not a correctness issue.

---

**C2. Path extension replacement edge cases**

The sidecar path is constructed by replacing the audio file extension
with `.lrc`. Verify the path construction handles:

1. **No extension:** `blob_location = "/music/track_no_ext"` →
   `Path::with_extension("lrc")` appends `.lrc` → `/music/track_no_ext.lrc`.
   This is probably correct behavior.

2. **Multiple dots:** `blob_location = "/music/artist.name/track.flac"` →
   `with_extension("lrc")` → `/music/artist.name/track.lrc"`. Verify
   only the final extension is replaced, not the dots in directory names.
   (`Path::with_extension` replaces only the last extension — correct.)

3. **`MEDIA_ROOT` prepending:** The plan says the adapter uses
   `MEDIA_ROOT` + `blob_location`. Verify there is no double-slash or
   missing slash at the join point:
   ```rust
   // Correct:
   PathBuf::from(&config.media_root).join(&blob_location)
   // Wrong:
   format!("{}{}", config.media_root, blob_location)  // no separator
   ```

4. **Absolute `blob_location`:** If `blob_location` is already an
   absolute path (starts with `/`), `PathBuf::join` discards the base
   and returns the absolute path as-is. This may or may not be the
   intended behavior — verify.

Severity: **Medium** — wrong sidecar path silently misses available
local `.lrc` files.

---

**C3. HTTP client reuse — reqwest client instantiation**

Verify that `LyricsAdapter` holds a single `reqwest::Client` instance
(created once at construction) rather than instantiating
`reqwest::Client::new()` on each `fetch_lyrics` call.

Each `reqwest::Client::new()` creates a new connection pool. Calling it
per-request:
- Prevents TCP connection reuse to `lrclib.net`
- Creates a new TLS session per request (expensive)
- Leaks resources if the client is not explicitly dropped

Run:
```bash
grep -n "Client::new\|reqwest::Client" \
    crates/adapters-lyrics/src/lib.rs
```

Expected: one `Client::new()` in the constructor/`new()` method,
stored as `self.client`. No `Client::new()` inside `fetch_lyrics`.

Severity: **Medium** — performance issue; not a correctness issue.

---

**C4. User-Agent version string — hardcoded vs dynamic**

The plan specifies `User-Agent: TeamTI/0.1.0`. This is hardcoded.

Verify whether the version is read from:
```rust
const UA: &str = concat!("TeamTI/", env!("CARGO_PKG_VERSION"));
```
or hardcoded as a string literal. The `env!("CARGO_PKG_VERSION")` macro
reads the version from `Cargo.toml` at compile time, ensuring the
User-Agent stays in sync with the actual version.

Also verify the User-Agent is set on the client at construction time
(via `reqwest::ClientBuilder::user_agent()`), not manually added as
a header on each request.

Severity: **Low** — cosmetic; hardcoded version will fall out of sync.

---

#### SECTION D — Pipeline Integration

**D1. TagWriter re-processing — lyrics written to file tags after fetch**

The plan states: "TagWriter will then neatly embed [lyrics] into the
.flac/.mp3 inside ID3/Vorbis Comments later."

Trace the actual execution path:

1. `LyricsWorker` calls `update_lyrics(track_id, lyrics)` → DB updated.
2. `LyricsWorker` emits `ToCoverArt` → `CoverArtWorker` runs →
   emits `ToTagWriter` (or equivalent).
3. `TagWriter` builds `TagData` from the DB `Track` record. At this point,
   `track.lyrics` should be populated (from step 1).
4. `TagData.lyrics` is set → embedded into the file.

The critical question: in step 3, does `TagWriter` re-read the `Track`
from DB? Or does it use the `Track` struct passed through the event chain,
which was populated BEFORE `update_lyrics` ran?

If the event carries a `Track` snapshot from before `update_lyrics`,
the lyrics will never be embedded in the file on this enrichment cycle.
They will only be embedded on the NEXT enrichment run (when the track
is re-scanned).

Verify by tracing `ToTagWriter` (or equivalent) event struct — does it
carry a `Track` snapshot or just a `track_id` that causes a fresh DB read?

Severity: **High** if event carries stale snapshot; **Low** if tag writer
always re-reads from DB.

---

**D2. `update_lyrics` called on already-enriched tracks — idempotency**

If a track runs through enrichment a second time (e.g. after `/rescan`),
`LyricsWorker` will:
1. Check `track.lyrics.is_some()` — already populated from the first run
2. Skip immediately, emit `ToCoverArt`

This is correct IF the skip check works as described. But if the skip
check uses the `ToLyrics` event's snapshot rather than a fresh DB read,
and the snapshot predates the first `update_lyrics` call, the worker
will attempt to fetch lyrics again and overwrite the existing value.

Verify the skip logic reads from a source that reflects the post-first-
enrichment DB state.

Severity: **Medium** — double-fetch is wasteful but not destructive
(overwrites with same or similar content).

---

**D3. Sidecar write-back — Open Question 2 resolution**

The plan's Open Question 2 asks whether to write a physical `.lrc` file
to disk when lyrics are fetched from LRCLIB.

This must be resolved before implementation is considered complete.
Evaluate:

**Write sidecar to disk:**
- Pros: media players (Dopamine, foobar2000, Poweramp) use sidecar `.lrc`
  for synchronized lyric display without needing embedded tags
- Cons: requires filesystem write access to `MEDIA_ROOT`; needs atomic
  write pattern (write to `.lrc.tmp`, rename to `.lrc`) to avoid partial
  files; adds complexity to `adapters-lyrics`

**DB only (no sidecar):**
- Pros: simpler; TagWriter handles the embedding naturally
- Cons: synchronized lyrics embedded as USLT lose their timing data
  (see B5); media players don't see the sidecar

**Recommendation to evaluate:**
For a personal NAS library where the primary lyric consumer is a future
Discord display, DB-only is simpler and sufficient for v2.1. Sidecar
write-back is a v3 feature. Verify whether the implementation matches
this or has already attempted sidecar writing (check for any
`tokio::fs::write` in `adapters-lyrics`).

Severity: **Medium** — design decision, not a bug.

---

#### SECTION E — Testing

**E1. Required tests — verify existence and completeness**

Run:
```bash
find . -path "*/target" -prune -o -name "*.rs" -print \
    | xargs grep -l "#\[test\]\|#\[tokio::test\]" 2>/dev/null \
    | grep -i "lyrics"
```

For each of the following tests, state: EXISTS / MISSING.
If missing, provide a concrete implementation proposal.

| Test | Verifies |
|------|----------|
| `lrclib_200_synced_lyrics` | Mock 200 with `syncedLyrics` populated → `Ok(Some(...))` |
| `lrclib_200_plain_only` | Mock 200 with `syncedLyrics: null`, `plainLyrics` present → falls back correctly |
| `lrclib_200_both_null` | Mock 200 with both null/empty → `Ok(None)` |
| `lrclib_404_returns_none` | Mock 404 → `Ok(None)`, not `Err` |
| `lrclib_429_returns_retryable_err` | Mock 429 → `Err` with `retryable = true` |
| `lrclib_album_name_none_omits_param` | `album_name = None` → HTTP request has no `album_name` param |
| `sidecar_found_skips_http` | `.lrc` file exists → returns sidecar content, no HTTP call made |
| `sidecar_not_found_falls_through` | No `.lrc` file → HTTP call made |
| `lyrics_worker_skips_if_already_present` | `track.lyrics.is_some()` → no `fetch_lyrics` call, emits `ToCoverArt` |
| `lyrics_worker_persists_on_match` | fetch returns `Some(lyrics)` → `update_lyrics` called with correct args |
| `lyrics_worker_continues_on_no_match` | fetch returns `Ok(None)` → emits `ToCoverArt` without calling `update_lyrics` |

For missing tests: specify mock setup, fixture data, and exact assertions.

---

### Diagnostic Commands

```bash
# 1. Build gate
cargo clippy --workspace --all-targets 2>&1

# 2. Tests
cargo test --workspace 2>&1

# 3. sqlx cache
cargo sqlx prepare --check --workspace 2>&1

# 4. std::fs usage in async adapters-lyrics (should be zero)
grep -rn "std::fs::" --include="*.rs" crates/adapters-lyrics/src/

# 5. reqwest::Client instantiation sites
grep -rn "Client::new\|reqwest::Client" \
    --include="*.rs" crates/adapters-lyrics/src/

# 6. update_lyrics in both port trait and concrete impl
grep -rn "update_lyrics" --include="*.rs" \
    crates/application/src/ports/ \
    crates/adapters-persistence/src/

# 7. Rate limiter in adapters-lyrics
grep -rn "rate_limit\|governor\|until_ready\|RateLimiter" \
    --include="*.rs" crates/adapters-lyrics/src/

# 8. 404 handling in LRCLIB adapter
grep -n "NOT_FOUND\|404\|StatusCode" \
    crates/adapters-lyrics/src/lib.rs

# 9. Null-safe response struct
grep -n "Option\|synced_lyrics\|plain_lyrics" \
    crates/adapters-lyrics/src/response.rs 2>/dev/null \
    || grep -n "Option\|synced\|plain" crates/adapters-lyrics/src/lib.rs

# 10. blob_location in port trait
grep -n "blob_location\|fetch_lyrics" \
    crates/application/src/ports/enrichment.rs

# 11. ToLyrics event struct fields
grep -A 10 "struct ToLyrics" \
    crates/application/src/events.rs

# 12. lyrics column type in migrations
grep -rn "lyrics" \
    crates/adapters-persistence/migrations/ --include="*.sql"

# 13. Tag writer lyrics path
grep -rn "Lyrics\|lyrics" \
    --include="*.rs" \
    crates/adapters-media-store/src/tag_writer.rs \
    crates/application/src/tag_writer_worker.rs

# 14. Test inventory for lyrics
find . -path "*/target" -prune -o -name "*.rs" -print \
    | xargs grep -l "lyrics\|lrclib\|LyricsWorker" 2>/dev/null \
    | grep -v "target/"

# 15. LYRICS_CONCURRENCY config key
grep -rn "LYRICS_CONCURRENCY\|lyrics_concurrency\|LyricsConcurrency" \
    --include="*.rs" crates/shared-config/src/
```

---

### Findings Report Format

```markdown
# Pass 1.1 Audit Report — LRCLIB Lyrics Integration

## Acceptance Gate
cargo clippy: PASS / FAIL
cargo test:   PASS / FAIL
sqlx check:   PASS / FAIL

---

## Diagnostic Output
[per-command, 1–15]

---

## Section A — Critical
### A1. LRCLIB response contract
Status: PASS / PARTIAL / FAIL
Evidence: <code quote>
Fix: ...
Classification: BLOCK / HIGH

[A2, A3...]

## Section B — High: Architecture
[B1–B5 per finding]

## Section C — Medium: Robustness
[C1–C4 per finding]

## Section D — Pipeline Integration
[D1–D3 per finding]

## Section E — Testing
### Test inventory
| Test | Status | Notes |

### Missing test proposals
[per test: mock setup / assertions / complexity]

---

## Open Question Resolutions

### OQ1: syncedLyrics vs plainLyrics storage format
Decision: <store synced as-is / strip timestamps / store both>
Rationale: ...
Required change (if any): ...

### OQ2: Sidecar write-back to disk
Decision: <DB only / also write .lrc / defer to v3>
Rationale: ...
Required change (if any): ...

---

## Summary

### BLOCK items
| ID | Issue | Fix |

### HIGH items
| ID | Issue | Fix |

### MEDIUM items
| ID | Issue |

### Missing tests (ship-blocking)
| ID | Test name | Complexity |

### Net rework scope
Files to change: N
Lines to add/modify: N
Lines to remove: N
```

---

### Constraints

- Do not apply fixes without approval.
- Exception: if `cargo test` fails due to a compile error in
  `adapters-lyrics`, fix the minimum to unblock diagnostics.
- Resolve both open questions from `LRCLIB.md` with concrete decisions,
  not deferral.
- Test proposals must include exact mock payloads and assertions —
  vague descriptions are rejected.
- If the implementation already satisfies a requirement, state PASS
  and move on.

---

### REFERENCE

https://lrclib.net/docs