# TeamTI v3 — Audit Pass 3.1
## User Layer: Correctness, Architecture & Completeness Review

> **Scope.** Full review of the Pass 3 output across all 7 layers.
> Fix all Critical and Major findings. Apply Optimizations unless they
> conflict with the Discord-first, no-background-worker constraint.
> Self-Explore items are open-ended — investigate, fix if broken,
> document if acceptable.
>
> **Attach:** all Pass 3 output files before sending.
> **Run first:** `cargo test --workspace` — report actual result
> independently of the walkthrough (see M5).

---

## Findings Index

| ID | Severity | Layer | Title |
|----|----------|-------|-------|
| C1 | Critical | Voice / Lifecycle | `TrackStarted` user list sourced from stale Discord cache |
| C2 | Critical | Lifecycle Worker | `close_listen_event` requires external event-ID state lost on crash |
| C3 | Critical | Voice / Lifecycle | `RadioRefillNeeded` carries no seed track ID — wrong recommendation path |
| M1 | Major | Persistence | Dangling open listen events on bot restart are never closed |
| M2 | Major | Discord Commands | `/playlist play` ordering — position ties are non-deterministic |
| M3 | Major | Discord Commands | `/favourite add` does not handle "nothing currently playing" |
| M4 | Major | All | 6 unused-variable warnings reported — must be zero |
| M5 | Major | Verification | `cargo test` status contradicted between walkthrough and task tracker |
| O1 | Optim. | Voice | Unbounded lifecycle channel — document and consider bounding |
| O2 | Optim. | Persistence | Recommendation genre overlap — verify index coverage |
| O3 | Optim. | Persistence | Large `exclude` list in recommendation query |
| S1 | Explore | Discord Commands | Pagination session timeout — verify no task leak |
| S2 | Explore | Persistence | `PLAYLIST_COLLABORATOR_LIMIT` — verify it is actually enforced |
| S3 | Explore | Persistence | Private playlist access — `NotFound` vs `Forbidden` contract |
| S4 | Explore | Discord Commands | `/playlist view` on a collaborator's playlist — verify access |
| S5 | Explore | Persistence | Reorder SQL — verify correct behaviour with duplicate positions |

---

## Critical Fixes

### C1 — `TrackStarted` user list sourced from stale Discord cache

**Files:** `play.rs`, `lifecycle_worker.rs`

**Problem.** The implementation enumerates voice channel members at the
moment `/play` is invoked — from the Discord gateway cache — and stores
those user IDs in `TrackStarted { guild_id, track_id, users_in_channel }`.
The lifecycle worker then opens one listen event per user in that list.

This has three failure modes:

1. **Stale cache.** If the bot recently joined a VC or the gateway
   event was dropped, the cache may not reflect the actual channel
   members. Members present in the VC but missing from cache get no
   listen event opened.

2. **Mid-track joins.** A user who joins the VC after the track starts
   is never enumerated and never gets a listen event, even if they
   listen to the entire track.

3. **Mid-track leaves.** A user who leaves the VC mid-track has an
   open event that will be closed at full duration, attributing a
   completed listen to them even though they left after 10 seconds.

**Fix.** The voice event handler already receives Songbird's voice state
events. Use `VoiceStateUpdate` events (which are reliable gateway events,
not cache reads) to maintain a `HashSet<UserId>` of current VC members
in `GuildMusicState`. This set is always authoritative:

```rust
// In GuildMusicState, add:
pub vc_members: HashSet<String>,  // Discord user IDs currently in VC

// In track_event_handler.rs / voice state handler:
// On VoiceStateUpdate where channel_id matches bot's channel:
//   user joined  → vc_members.insert(user_id)
//                → if a track is playing, open a listen event for them
//   user left    → vc_members.remove(user_id)
//                → close their open listen event with elapsed duration
```

For `TrackStarted`, snapshot `state.vc_members` at the moment the track
actually begins playing (inside the voice adapter), not at `/play` invocation.
These are different instants — there can be a delay between command and playback.

---

### C2 — `close_listen_event` requires external event-ID state lost on crash

**Files:** `lifecycle_worker.rs`, `user_library_repository.rs`

**Problem.** `UserLibraryPort::open_listen_event` returns `Uuid` (the
event row ID). `close_listen_event` takes that UUID to update the row.
The lifecycle worker must therefore maintain a map:

```rust
// Somewhere in the lifecycle worker:
HashMap<(GuildId, TrackId, UserId), Uuid>   // event_id
```

This map lives only in memory. If the bot crashes, restarts, or the
lifecycle worker task panics, the map is lost — and all open events
can never be closed through the normal path.

**Fix.** Change `close_listen_event` to close by identity fields, not
by a stored ID:

```rust
// Replace the port method signature:
async fn close_listen_event(
    &self,
    user_id:          &str,
    track_id:         Uuid,
    play_duration_ms: i32,
    track_duration_ms: i32,
) -> Result<(), AppError>;

// SQL:
UPDATE listen_events
SET
    play_duration_ms = $3,
    completed        = ($3::float / NULLIF($4, 0)) >= $5
WHERE user_id    = $1
  AND track_id   = $2
  AND play_duration_ms IS NULL   -- only close open events
```

The `play_duration_ms IS NULL` filter closes all open events for that
user × track pair. This is idempotent and requires no external state.
The lifecycle worker no longer needs to track event IDs.

Update the port trait, repository impl, and all call sites.

---

### C3 — `RadioRefillNeeded` carries no seed track ID

**Files:** `lifecycle.rs`, `track_event_handler.rs`, `lifecycle_worker.rs`

**Problem.** The `RadioRefillNeeded { guild_id, user_id }` event carries
no information about which track to use as the seed for recommendations.
When the lifecycle worker receives it and calls `RecommendationPort::recommend`,
it must pass `seed_track_id: Option<Uuid>`. Without the seed, it passes
`None`, which falls back to profile-only recommendations. The design
decision was that radio seeds from the **current track** — this is
silently violated.

**Fix.** Carry the seed track ID in the event:

```rust
// In lifecycle.rs:
pub enum TrackLifecycleEvent {
    TrackStarted {
        guild_id:         String,
        track_id:         Uuid,
        users_in_channel: Vec<String>,
    },
    TrackEnded {
        guild_id:         String,
        track_id:         Uuid,    // the track that just ended
        play_duration_ms: i32,
    },
    RadioRefillNeeded {
        guild_id:    String,
        user_id:     String,
        seed_track_id: Uuid,       // ← ADD THIS
    },
}
```

In `track_event_handler.rs`, when emitting `RadioRefillNeeded`, pass
the ID of the track that just finished as the seed — this is the
`track_id` that was playing, already available in the handler's context.

---

## Major Fixes

### M1 — Dangling open listen events on bot restart

**Files:** `main.rs`, `user_library_repository.rs`

**Problem.** When the bot stops (graceful or crash), any
`listen_events` rows with `play_duration_ms IS NULL` remain open
indefinitely. On the next startup, these rows are never closed —
they accumulate silently, corrupting listen history and recommendation
scoring. A user with 3 dangling open events per week has dozens of
ghost completions skewing their affinity profile within a month.

**Fix.** On startup, before spawning any workers, close all dangling
events with a sweep query:

```rust
// In UserLibraryPort trait, add:
async fn close_dangling_events(&self, older_than_secs: i64) -> Result<u64, AppError>;

// SQL:
UPDATE listen_events
SET
    play_duration_ms = 0,
    completed        = false
WHERE play_duration_ms IS NULL
  AND started_at < NOW() - make_interval(secs => $1)
RETURNING id
```

In `main.rs`, call this before the lifecycle worker is spawned:

```rust
let closed = user_library_port
    .close_dangling_events(3600)  // events open > 1 hour are stale
    .await
    .expect("failed to close dangling listen events");

if closed > 0 {
    tracing::warn!(
        count     = closed,
        operation = "listen_events.startup_cleanup",
        "closed dangling listen events from previous session"
    );
}
```

The `older_than_secs = 3600` threshold avoids closing an event that
legitimately opened in the last hour (e.g., if the bot was restarted
quickly during a track). Adjust if the longest track in the library
exceeds 1 hour.

---

### M2 — `/playlist play` ordering — position ties are non-deterministic

**File:** `playlist_repository.rs`

**Problem.** When reordering tracks, two items can temporarily (or
permanently, if the reorder logic is naive) have the same position
value. `/playlist play` queues tracks by position — without a
secondary sort, tie-breaking is non-deterministic and varies between
query executions on the same data.

**Fix.** Ensure the query driving `/playlist play` and `get_playlist_items`
uses a deterministic secondary sort:

```sql
-- Always use this ORDER BY in any playlist item query:
ORDER BY pi.position ASC, pi.added_at ASC
```

Audit every query in `playlist_repository.rs` that iterates playlist
items and confirm all of them use this compound sort. A single
`ORDER BY position` anywhere produces a non-deterministic playlist.

---

### M3 — `/favourite add` does not handle "nothing currently playing"

**File:** `commands/favourite.rs`

**Problem.** `/favourite add` with no track argument defaults to the
currently playing track. If nothing is playing in the guild (bot not in
VC, or queue is empty), the command must handle this gracefully. Likely
failure modes if unhandled:
- Panics on `Option::unwrap()` of current track
- Returns a confusing generic error embed
- Silently adds nothing to favourites

**Fix.** Check whether a track is currently playing before attempting
to favourite it:

```rust
// In the /favourite add handler:
let track_id = if let Some(arg_track) = track_argument {
    // User provided a track via autocomplete
    arg_track
} else {
    // Default to currently playing
    let current = get_current_track(&guild_state);
    match current {
        Some(t) => t.id,
        None => {
            return respond_ephemeral(
                ctx, interaction,
                "Nothing is currently playing. Search for a track to favourite."
            ).await;
        }
    }
};
```

The response must be ephemeral — it's personal feedback.

---

### M4 — 6 unused-variable warnings must be zero

**Files:** Various (agent to locate via `cargo build`)

**Problem.** The walkthrough reports "6 expected unused-variable warnings"
as if they are acceptable. They are not. The project builds with
`-D warnings` for all non-test code (or should). Warnings are bugs
waiting to happen and degrade the signal-to-noise ratio of `cargo build`
output, making real warnings invisible.

**Fix.** Run:
```bash
cargo build --workspace 2>&1 | grep "^warning"
```

For each warning:
- If the variable is genuinely unused, prefix with `_` (`_result`,
  `_event_id`, etc.) or remove it entirely.
- If it should be used but isn't (e.g., a returned `Uuid` from
  `open_listen_event` that was meant to be stored), fix the logic.
- Do not `#[allow(unused_variables)]` — that hides the warning without
  fixing it.

Zero warnings is the acceptance criterion for this finding.

---

### M5 — `cargo test` status contradicted between walkthrough and task tracker

**Files:** N/A — verification issue

**Problem.** The task tracker (`task-2.md`) shows:
```
- [ ] cargo test --workspace passes
```
(unchecked). The walkthrough table shows `✅ All tests pass`.

One of these is wrong. A partially completed test suite that was
retroactively marked as passing is a silent regression risk.

**Fix.** Run `cargo test --workspace` fresh from a clean build and
report the actual output:

```bash
cargo test --workspace 2>&1 | tail -20
```

If any tests fail, fix them before proceeding with other findings.
If all pass, update the task tracker to reflect reality.

---

## Optimizations

### O1 — Unbounded lifecycle channel

**File:** `lifecycle.rs`

`mpsc::UnboundedSender` has no backpressure. In a high-concurrency
scenario (many guilds, many tracks starting/ending simultaneously),
if the lifecycle worker is blocked on a slow DB call, the channel
buffer grows without bound. For the current small-scale Discord-first
deployment this is unlikely to cause problems, but it is a correctness
risk at scale.

**Fix for Pass 3.1:** Add a comment documenting the known risk:

```rust
// UnboundedSender is used here because lifecycle events are low-volume
// (one per track start/end) and the worker processes them quickly.
// If this bot scales to many concurrent guilds or the DB becomes a
// bottleneck, switch to mpsc::channel with a bounded buffer and add
// backpressure handling. TODO: revisit in v4 server architecture.
```

**Optional immediate fix:** Switch to `mpsc::channel(512)` and handle
send errors by logging and dropping the event (which is preferable to
an OOM crash):

```rust
// In lifecycle_worker.rs consumer:
// If send returns Err (channel full), log and continue:
if let Err(e) = lifecycle_tx.send(event) {
    tracing::warn!(
        operation = "lifecycle.channel_full",
        "lifecycle event dropped — channel full: {e}"
    );
}
```

---

### O2 — Recommendation genre overlap — verify index coverage

**File:** `recommendation_repository.rs`

Genre overlap in the CTE-based recommendation scoring likely uses a
PostgreSQL array overlap (`&&`) or `UNNEST` intersection. Neither of
these operators uses the GIN index on `tracks.genres` efficiently
when the filter is part of a complex CTE scoring expression.

**Agent task:** Inspect the recommendation SQL query. For the genre
scoring component, run `EXPLAIN ANALYZE` on it with a realistic user
genre array. If the plan shows a sequential scan on `tracks` for the
genre overlap:

Option A — Accept the sequential scan if the `tracks` table is small
(<50k rows) and query time is <100ms. Document with a TODO.

Option B — Extract the genre filter as a pre-filter CTE before
scoring:

```sql
WITH user_genres AS (
    -- Aggregate user's top genres from listen history
    ...
),
candidate_tracks AS (
    -- Use the GIN index to get genre-matching tracks first
    SELECT t.id FROM tracks t
    WHERE t.genres && (SELECT array_agg(genre) FROM user_genres)
      AND t.id != $seed_track_id
      AND t.id != ALL($exclude)
),
scored AS (
    -- Score only the genre-matched candidates
    ...
)
```

The GIN index on `tracks.genres` (from `0004_indexes.sql`) is only
used when `tracks.genres` is the left-hand side of a `&&` operator in
a WHERE clause, not inside a scoring expression.

---

### O3 — Large `exclude` list degrades recommendation query

**File:** `recommendation_repository.rs`

`recommend(user_id, seed, exclude: &[Uuid], limit)` passes the
exclusion list as `$exclude = ANY(...)`. For a large playlist just
queued (e.g., 200 tracks), this list can be 200+ UUIDs. PostgreSQL
handles this fine, but the query plan should not degrade with list size.

**Agent task:** Test the recommendation query with `exclude` lists of
size 0, 25, 100, and 500. If execution time scales linearly with
exclude size (indicating a nested loop scan), add a materialized CTE
for the exclusion filter:

```sql
WITH excluded AS (
    SELECT UNNEST($3::uuid[]) AS id
)
-- Then use: AND t.id NOT IN (SELECT id FROM excluded)
-- instead of: AND t.id != ALL($3::uuid[])
```

`NOT IN (subquery)` with a materialized CTE typically produces a
hash anti-join, which is O(1) lookup vs O(N) for `= ALL(array)`.

---

## Self-Explore Items

These require the agent to read the implementation and make a
judgement call. Fix if broken, document if intentionally accepted.

### S1 — Pagination session timeout: verify no task leak

**File:** `pagination.rs`

The design spec requires pagination buttons to be removed after 5
minutes. Verify how this is implemented. The two common approaches are:

- **A. `tokio::spawn` per session** — spawns a sleep task that edits
  the message after 5 minutes. If the user deletes the message or
  navigates away, the task still sleeps and fires against a
  potentially invalid message. Verify the edit failure is handled
  gracefully (not a panic).

- **B. Timestamp in custom_id** — the button `custom_id` encodes a
  creation timestamp. On each button press, check if `now > created + 5min`
  and reject with an ephemeral "session expired" response. No background
  tasks needed. This is the preferred approach.

If approach A is used, verify:
1. The spawned task handles the message-not-found edit failure
2. The tasks are not accumulated without bound (each navigation
   creating a new sleep task)

If approach B is not used, consider migrating to it.

---

### S2 — `PLAYLIST_COLLABORATOR_LIMIT` enforcement

**File:** `playlist_repository.rs`

`application::PLAYLIST_COLLABORATOR_LIMIT` is defined as a constant.
Verify that `PlaylistPort::add_collaborator` actually checks it:

```rust
// Expected logic:
let count = // SELECT COUNT(*) FROM playlist_collaborators WHERE playlist_id = $1
if count >= PLAYLIST_COLLABORATOR_LIMIT {
    return Err(AppError::Playlist {
        kind:   PlaylistErrorKind::CollaboratorLimit,
        detail: format!("limit is {}", PLAYLIST_COLLABORATOR_LIMIT),
    });
}
```

If this check is absent, a playlist can accumulate unlimited
collaborators. Verify the check exists and is tested.

Also verify: what is the value of `PLAYLIST_COLLABORATOR_LIMIT`? If
it is `usize::MAX` or `i64::MAX` as a placeholder, set a reasonable
default (e.g., `10`) and document it.

---

### S3 — Private playlist access: `NotFound` vs `Forbidden`

**File:** `playlist_repository.rs`

The design spec requires that a non-owner, non-collaborator accessing
a private playlist receives `AppError::Playlist { kind: NotFound }`,
NOT `Forbidden` — to avoid leaking the existence of private playlists.

Verify this is implemented correctly in every read operation:
`get_playlist_items`, `get_playlist_tracks`, and any autocomplete
query that lists playlists.

The pattern to check for:
```rust
// CORRECT — returns NotFound regardless of reason:
if !has_access { return Err(AppError::Playlist { kind: NotFound, ... }) }

// INCORRECT — leaks existence:
if !is_owner && !is_collaborator {
    if playlist.visibility == Private {
        return Err(NotFound)   // ok
    } else {
        return Err(Forbidden)  // wrong — only owners can write
    }
}
```

Also check: what does `/playlist list @other_user` return if the other
user has only private playlists? It should return an empty list, not
an error.

---

### S4 — `/playlist view` on another user's public playlist

**File:** `commands/playlist.rs`, `playlist_repository.rs`

A key UX feature is that public playlists are visible to everyone.
Verify the full flow works:

1. User A creates a playlist and makes it public with `/playlist share`
2. User B (different Discord user, same server) runs `/playlist view`
   and can see User A's public playlist in autocomplete
3. User B browses User A's playlist — can navigate pages
4. User B tries `/playlist add` on User A's playlist — should receive
   `Forbidden` (not `NotFound`, since it IS public — existence is not
   secret for public playlists)

Verify the autocomplete for `/playlist view` includes public playlists
from other users, not just the invoking user's own playlists.

---

### S5 — Reorder SQL with duplicate position values

**File:** `playlist_repository.rs`

The position column has no UNIQUE constraint (intentionally dropped in
Pass 3). When `reorder_track(playlist_id, item_id, new_position)` is
called, verify what SQL is executed.

The naïve implementation:
```sql
UPDATE playlist_items SET position = $3 WHERE id = $2
```

This is correct for moving a single item — it creates a tie at the
target position, but the secondary `added_at` sort resolves it.

The problematic implementation would be one that tries to shift other
items to make room (e.g., `UPDATE ... SET position = position + 1 WHERE
position >= $new_position`). This is unnecessary given the soft-ordering
design, but if attempted, it could affect ALL items at that position in
ALL playlists if the query lacks a `playlist_id` filter.

Verify the reorder SQL:
1. Uses a `WHERE id = $item_id` filter (not just `position = $old_position`)
2. Does NOT attempt to shift other items
3. Has a `playlist_id` check to prevent cross-playlist modification

---

## Verification Checklist

```bash
# 1. Fresh from the start:
cargo sqlx database drop && cargo sqlx database create
cargo sqlx migrate run
cargo sqlx prepare --workspace

# 2. Zero warnings:
cargo build --workspace 2>&1 | grep "^warning"
# Expected: no output

# 3. Tests — report actual result:
cargo test --workspace

# 4. Dangling event cleanup fires on startup:
# Insert a row: INSERT INTO listen_events VALUES (gen_random_uuid(),
#   'test_user', (SELECT id FROM tracks LIMIT 1), 'guild1',
#   NOW() - INTERVAL '2 hours', NULL, false);
# Start the bot, check logs for: listen_events.startup_cleanup count=1

# 5. Radio seed is correct:
# /play a track → /radio → let queue drain
# Check logs: recommendation should include seed_track_id (not null)

# 6. Private playlist not-found:
# User A creates private playlist
# User B: /playlist view → A's playlist should NOT appear in autocomplete
# User B: direct API call with A's playlist ID → AppError::Playlist(NotFound)

# 7. Collaborator limit:
# Owner adds PLAYLIST_COLLABORATOR_LIMIT + 1 collaborators
# Last add should return: AppError::Playlist(CollaboratorLimit)
```
