# TeamTI v3 — Audit Pass 5.1
## Queue, Skip, Pause/Resume, NP: Type Safety, Cast Review & UX Correctness

> **Scope.** Full review of Pass 5 output. Read every modified file in
> `adapters-voice/src/state.rs`, `track_event_handler.rs`,
> `lifecycle_worker.rs`, and all new/modified files in `adapters-discord`.
> Fix all Critical and Major findings. Apply all Refactor items.
> Self-Explore items require reading implementation — fix if broken.
>
> **Schema ground truth** (confirmed from actual migration files):
> - `tracks.duration_ms  INTEGER`  — PostgreSQL `INTEGER` = sqlx `i32`
> - `listen_events`      — no `play_duration_ms` or `track_duration_ms`
>   columns in base schema (verify they were added in a Pass 3+ migration)
> - `tracks.file_size_bytes  BIGINT` — sqlx `i64`
> - All durations stored in milliseconds (INTEGER / i32)

---

## Findings Index

| ID | Severity | Area | Title |
|----|----------|------|-------|
| C1 | Critical | `state.rs` | `as_millis() as u32` — lossy cast from `u128`, overflows at ~49 days |
| C2 | Critical | `state.rs` / `QueueEntry` | `duration_ms: u32` mismatches DB `INTEGER` (i32) — sqlx type error |
| C3 | Critical | Schema | `listen_events` missing `play_duration_ms` / `track_duration_ms` — verify migration exists |
| C4 | Critical | `lifecycle_worker.rs` | Auto-leave cleanup does not cancel NP update token |
| M1 | Major | `state.rs` | `total_paused_ms: u32` accumulates `u128` millis — right type, wrong ceiling |
| M2 | Major | `track_event_handler.rs` | Completion ratio cast `as f32` — loses precision for long tracks, use `f64` |
| M3 | Major | Multiple | `queue_meta` sync missing from pre-Pass 5 enqueue paths (`/playlist play`, `/play`) |
| M4 | Major | Commands | `/skip N` — no bounds check; N ≥ queue length panics or silently no-ops |
| M5 | Major | Commands | `/queue move` / `/queue remove` — no guard against position 0 (currently playing) |
| M6 | Major | `state.rs` | `duration_ms: Option<i32>` from DB — NULL tracks crash progress bar and queue display |
| R1 | Refactor | `adapters-discord` | `format_duration_ms` and `progress_bar` duplicated — extract to `ui` module |
| R2 | Refactor | `adapters-discord` | Embed color literals scattered — extract to named constants |
| R3 | Refactor | `adapters-discord` | `custom_id` built/parsed with raw string ops — replace with structured type |
| R4 | Refactor | `adapters-discord` | Embed builders inline in command handlers — extract to builder functions |
| O1 | Optim. | `lifecycle_worker.rs` | NP auto-update fires on edit even when bot is paused — minor wasted API calls |
| S1 | Explore | Commands | `/queue save` when queue is empty — edge case response missing |
| S2 | Explore | `adapters-discord` | Discord component 15-minute timeout — no graceful handling |
| S3 | Explore | `state.rs` | NP task orphan on crash/restart — document and accept |
| S4 | Explore | `track_event_handler.rs` | `TrackEnded` fires on skip AND natural end — verify both paths set correct duration |

---

## Critical Fixes

### C1 — `as_millis() as u32`: lossy cast from `u128`

**File:** `crates/adapters-voice/src/state.rs`, `track_event_handler.rs`

**Problem.** The Pass 5 design spec introduced this pattern:

```rust
// On /resume:
let paused_ms = pa.elapsed().as_millis() as u32;
state.total_paused_ms += paused_ms;

// On TrackEnded:
let elapsed_ms = state.track_play_started_at
    .map(|s| s.elapsed().as_millis() as u32)
    .unwrap_or(0);
```

`Duration::as_millis()` returns `u128`. Casting to `u32` silently
overflows and wraps at 2^32 ms = **49.7 days**. While no individual
track lasts that long, `track_play_started_at` is set at `TrackStarted`
and only reset on the next `TrackStarted`. If the bot stays connected
with the queue paused (or the NAS goes offline) for >49 days, `elapsed_ms`
wraps to 0 and the listen event records 0ms — marking every subsequent
track as incomplete.

More practically, `total_paused_ms: u32` overflows after ~49 days of
cumulative pause time across a session. Again unlikely, but wrong by
design.

**Fix.** Use `u64` for all in-memory millisecond duration fields and
intermediate computations. `u64` holds up to 585 million years.

```rust
// GuildMusicState — corrected field types:
pub total_paused_ms: u64,

// On /resume:
let paused_ms = pa.elapsed().as_millis() as u64;  // u128 → u64: safe until year 584,554,049
state.total_paused_ms = state.total_paused_ms.saturating_add(paused_ms);

// On TrackEnded:
let elapsed_ms: u64 = state.track_play_started_at
    .map(|s| s.elapsed().as_millis() as u64)
    .unwrap_or(0);
let actual_play_ms: u64 = elapsed_ms.saturating_sub(state.total_paused_ms);
```

When writing `actual_play_ms` to the database (as `play_duration_ms
INTEGER / i32`), cap it before casting:

```rust
// Safe narrowing: realistic track durations fit in i32 (max ~596 hours).
// saturating_cast guards against any edge-case overflow.
let play_duration_db: i32 = actual_play_ms.min(i32::MAX as u64) as i32;
```

---

### C2 — `QueueEntry::duration_ms: u32` mismatches DB `INTEGER` (i32)

**File:** `crates/adapters-voice/src/state.rs` or wherever `QueueEntry` is defined

**Problem.** The schema has:
```sql
tracks.duration_ms  INTEGER   -- PostgreSQL INTEGER = Rust i32 via sqlx
```

sqlx's `query!` macro maps `INTEGER` to `i32`. Any code that reads
`duration_ms` into a `u32` field either fails to compile (if using
compile-time checked `query!`) or panics at runtime (if using `query_as!`
with the wrong type).

The design spec specified `duration_ms: u32` in `QueueEntry` — this was
wrong relative to the actual schema.

**Fix.** Change `QueueEntry.duration_ms` to `i32`. All downstream uses
must be updated:

```rust
pub struct QueueEntry {
    pub track_id:    uuid::Uuid,
    pub title:       String,
    pub artist:      String,
    pub duration_ms: i32,        // matches tracks.duration_ms INTEGER
    pub added_by:    String,
    pub source:      QueueSource,
}
```

When computing elapsed time comparisons (e.g., completion threshold),
cast explicitly:

```rust
// Completion ratio — both values from the DB are i32:
let completed = if track_duration_ms > 0 {
    (actual_play_ms as f64) / (track_duration_ms as f64)
        >= LISTEN_COMPLETION_THRESHOLD as f64
} else {
    false
};
```

When summing durations for queue "~N min remaining":

```rust
let total_remaining_ms: i64 = queue_meta
    .iter()
    .skip(1)  // skip currently playing
    .map(|e| e.duration_ms as i64)  // widen to i64 before sum to avoid overflow
    .sum();
let total_remaining_min = total_remaining_ms / 60_000;
```

Note: `duration_ms` is `Option<INTEGER>` in the schema (`duration_ms
INTEGER` without NOT NULL). See M6 for handling NULL.

---

### C3 — `listen_events` missing `play_duration_ms` / `track_duration_ms`

**File:** `migrations/` — verify all migration files

**Problem.** The base schema `0003_user_library-3.sql` defines
`listen_events` as:

```sql
CREATE TABLE IF NOT EXISTS listen_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     TEXT NOT NULL,
    track_id    UUID NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    guild_id    TEXT NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed   BOOLEAN NOT NULL DEFAULT false
);
```

There is **no `play_duration_ms` or `track_duration_ms` column**. Yet
the Pass 5 implementation calls `close_listen_event(user_id, track_id,
actual_play_ms, track_duration_ms)` and the completion ratio formula
requires both values. Either:

1. These columns were added in a Pass 3 or Pass 3.1 migration that was
   not attached to this audit (likely), OR
2. The completion calculation happens in Rust and only `completed` is
   stored in the DB (also possible — `completed` is the only boolean
   needed for the recommendation engine)

**Fix — verification steps:**

```bash
# List all migration files:
ls migrations/

# Search for play_duration in all migrations:
grep -r "play_duration" migrations/

# Check the close_listen_event port signature:
grep -r "close_listen_event" crates/application/src/ports/
```

If `play_duration_ms` and `track_duration_ms` columns **do not exist**,
add a migration:

```sql
-- migrations/0006_listen_duration-6.sql
ALTER TABLE listen_events
    ADD COLUMN IF NOT EXISTS play_duration_ms INTEGER,
    ADD COLUMN IF NOT EXISTS track_duration_ms INTEGER;
-- Both nullable: NULL = event not yet closed or duration unknown
```

If the columns **already exist in a later migration**, confirm the
Rust types match (`i32` for `INTEGER`). The pause-adjusted
`actual_play_ms: u64` must be narrowed to `i32` before the INSERT/UPDATE
(per C1's fix above).

---

### C4 — Auto-leave cleanup does not cancel NP update token

**File:** `crates/adapters-voice/src/track_event_handler.rs` (auto-leave logic)

**Problem.** The auto-leave timer (`AUTO_LEAVE_SECS`) disconnects the
bot when the VC has been empty for the configured duration. The `/leave`
command correctly cancels `np_update_cancel`. But the **auto-leave code
path** in `track_event_handler.rs` bypasses the same cleanup, because
it calls Songbird's disconnect directly rather than going through the
`/leave` command handler.

Result: after auto-leave, the NP update task keeps running, attempts to
edit a message every 5 seconds, gets HTTP 404 errors (message deleted
by user) or 403 errors, and spams error logs indefinitely.

**Fix.** Extract the leave cleanup into a shared function called by
both `/leave` and the auto-leave path:

```rust
// In a shared location (e.g., adapters-voice/src/state.rs or a new leave.rs):
pub async fn cleanup_on_leave(state: &mut GuildMusicState) {
    // 1. Cancel NP update task
    if let Some(cancel) = state.np_update_cancel.take() {
        cancel.cancel();
    }
    // 2. Clear NP message reference
    state.nowplaying_message_id = None;
    // 3. Reset pause tracking
    state.paused_at = None;
    state.total_paused_ms = 0;
    state.track_play_started_at = None;
    // 4. Clear queue metadata
    state.queue_meta.clear();
    // 5. last_play_channel_id: intentionally preserved
    //    (so a /play after rejoin posts to the same channel)
}
```

Call `cleanup_on_leave(&mut state)` from BOTH the `/leave` command
handler AND the auto-leave timer in `track_event_handler.rs`.

---

## Major Fixes

### M1 — `total_paused_ms: u32` — correct fix is part of C1

**Covered by C1.** Change to `u64`. Use `saturating_add` to accumulate.

---

### M2 — Completion ratio cast `as f32` loses precision for long tracks

**File:** `crates/adapters-voice/src/track_event_handler.rs`

**Problem.**

```rust
// Potentially in the existing codebase from Pass 3:
let completed = (play_duration_ms as f32 / track_duration_ms as f32)
    >= LISTEN_COMPLETION_THRESHOLD;
```

`f32` has 23 bits of mantissa — it can represent integers exactly up
to 2^23 = 8,388,608. A track duration of ~2.3 hours (8,388,608 ms) is
where `f32` precision starts degrading. A 3-hour classical symphony or
live recording at 10,800,000 ms cannot be represented exactly in `f32`,
causing the ratio to be slightly off.

**Fix.** Use `f64` throughout all ratio calculations involving durations.
`f64`'s 52-bit mantissa handles integers exactly up to 2^52 — far beyond
any realistic track length.

```rust
let ratio = if track_duration_ms > 0 {
    (play_duration_ms as f64) / (track_duration_ms as f64)
} else {
    0.0_f64
};
let completed = ratio >= LISTEN_COMPLETION_THRESHOLD as f64;
```

---

### M3 — `queue_meta` sync missing from pre-Pass 5 enqueue paths

**Files:** `crates/adapters-discord/src/commands/play.rs`,
`crates/adapters-discord/src/commands/playlist.rs`

**Problem.** `queue_meta: Vec<QueueEntry>` was introduced in Pass 5.
But `/play` and `/playlist play` were written in earlier passes and
pre-date `queue_meta`. They enqueue tracks via Songbird's `TrackQueue`
but do NOT push corresponding `QueueEntry` objects to `queue_meta`.

Result: the queue embed shows an empty or stale list while music plays.
The currently playing track is absent from the metadata list. Position
numbers are wrong. `/skip <name>` autocomplete returns nothing.

**Fix.** In every location where a `TrackHandle` is pushed to Songbird's
queue, also push a `QueueEntry` to `queue_meta`:

```rust
// In /play command handler, after enqueueing:
let entry = QueueEntry {
    track_id:    track.id,
    title:       track.title.clone(),
    artist:      track.artist_display.clone().unwrap_or_default(),
    duration_ms: track.duration_ms.unwrap_or(0),
    added_by:    ctx.author().id.to_string(),
    source:      QueueSource::Manual,
};
state.queue_meta.push(entry);

// In /playlist play, for each track in the playlist:
state.queue_meta.push(QueueEntry {
    ...
    source: QueueSource::Manual,  // playlist tracks = Manual per Q14
});

// In radio refill (lifecycle_worker.rs RadioRefillNeeded handler):
state.queue_meta.push(QueueEntry {
    ...
    source: QueueSource::Radio,   // radio tracks = Radio per Q13
});
```

Also ensure that when `TrackEnded` fires and Songbird automatically
advances the queue, `queue_meta.remove(0)` is called to keep position
0 = currently playing in sync.

---

### M4 — `/skip N`: no bounds check — out-of-range N panics or silently fails

**File:** `crates/adapters-discord/src/commands/skip.rs`

**Problem.** `/skip 99` on a 3-track queue. If the implementation does:

```rust
for _ in 0..(n - 1) {
    track_queue.dequeue(1);         // could panic or no-op at end of queue
    queue_meta.remove(1);           // panics if index out of bounds
}
track_queue.skip();
```

`Vec::remove(index)` panics on out-of-bounds. `TrackQueue::dequeue`
returns `Option` but silently returns `None` after the last track —
leaving Songbird's queue and `queue_meta` desynchronised.

**Fix.** Validate N before any mutation:

```rust
pub async fn handle_skip(
    ctx:     &Context,
    guild_id: GuildId,
    target:  SkipTarget,   // SkipTarget::Next | SkipTarget::ToPosition(usize)
) -> Result<(), AppError> {
    let state = get_guild_state(guild_id).await?;
    let mut state = state.lock().await;

    let queue_len = state.queue_meta.len();

    match target {
        SkipTarget::Next => {
            if queue_len <= 1 {
                return Err(AppError::Command("Queue is empty after skip.".into()));
            }
            // skip normally
        }
        SkipTarget::ToPosition(n) => {
            // n is 1-based (position in queue embed)
            if n == 0 || n >= queue_len {
                return Err(AppError::Command(
                    format!("Position {} doesn't exist in the queue.", n)
                ));
            }
            // discard positions 1..n-1, then skip to n
            for _ in 1..n {
                state.track_queue.dequeue(1);
                state.queue_meta.remove(1);
            }
            state.track_queue.skip();
            state.queue_meta.remove(0);
        }
    }
    Ok(())
}
```

---

### M5 — `/queue move` / `/queue remove` must guard against position 0

**File:** `crates/adapters-discord/src/commands/queue.rs`

**Problem.** Position 0 = currently playing track. If a user calls
`/queue remove 1` (which in 1-based display maps to position 0 in
the vec), or the implementation doesn't offset the 1-based user input
to 0-based indices correctly, the currently playing track is removed
from `queue_meta` without stopping it in Songbird. The bot then plays
the invisible track to completion, then tries to advance to a
desynchronised next track.

**Fix.** Two issues to resolve:

**1. Display vs internal index alignment.**
In the queue embed, position 1 = currently playing (index 0 in `queue_meta`).
Positions 2, 3... = indices 1, 2... All user-facing 1-based positions
must be converted: `internal_idx = user_position - 1`.

**2. Guard position 0 for remove and move.**

```rust
// /queue remove validation:
if internal_idx == 0 {
    return Err(AppError::Command(
        "Can't remove the currently playing track. Use /skip instead.".into()
    ));
}

// /queue move validation:
if from_idx == 0 || to_idx == 0 {
    return Err(AppError::Command(
        "Can't move the currently playing track.".into()
    ));
}
if from_idx == to_idx {
    return Ok(());  // no-op, no response needed or send "Already at that position."
}
if from_idx >= queue_meta.len() || to_idx >= queue_meta.len() {
    return Err(AppError::Command(
        format!("Position {} doesn't exist.", from_idx + 1)
    ));
}
```

---

### M6 — `duration_ms: Option<INTEGER>` — NULL tracks crash display

**Files:** `crates/adapters-discord/src/ui/embeds.rs` (or equivalent)

**Problem.** `tracks.duration_ms` is `INTEGER` without `NOT NULL` in
the schema — it is `Option<i32>` when read via sqlx. Some tracks
(e.g. freshly scanned before metadata extraction, or tracks with
malformed files) have `duration_ms = NULL`.

The progress bar computation divides by `total_ms`. If `duration_ms`
is NULL, `total_ms = 0`, and the design spec's guard handles it:

```rust
if total_ms == 0 { return "─".repeat(width); }
```

But `QueueEntry.duration_ms: i32` as written in C2's fix cannot store
NULL. Either:
- `QueueEntry.duration_ms` must be `Option<i32>` to faithfully represent
  the DB value, OR
- Substitute 0 at read time: `duration_ms: track.duration_ms.unwrap_or(0)`
  and document that 0 means "unknown"

**Fix.** Use 0-as-sentinel in `QueueEntry` (simpler, avoids Option
propagation through display code):

```rust
pub struct QueueEntry {
    pub duration_ms: i32,  // 0 = unknown/NULL from DB
    // ...
}

// When reading from DB:
duration_ms: track.duration_ms.unwrap_or(0),
```

In ALL display code, check for 0 and show placeholder:

```rust
fn format_duration(ms: i32) -> String {
    if ms <= 0 { return "--:--".to_string(); }
    let total_secs = (ms as u32) / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}
```

Similarly, `total_remaining_ms` calculation must skip tracks with
`duration_ms == 0` (or treat them as 0 — they contribute nothing to
the sum, which is correct):

```rust
let total_remaining_ms: i64 = queue_meta
    .iter()
    .skip(1)
    .filter(|e| e.duration_ms > 0)
    .map(|e| e.duration_ms as i64)
    .sum();
```

---

## Refactor Items

### R1 — `format_duration` and `progress_bar` duplicated across files

**Problem.** These functions are likely present in:
- `nowplaying.rs` (for the NP embed)
- `queue.rs` (for per-track duration display)
- Potentially `autocomplete.rs` (for skip autocomplete display)

Duplicated utility functions drift apart silently — one version handles
`ms <= 0` and another doesn't.

**Fix.** Create `crates/adapters-discord/src/ui/mod.rs`:

```
crates/adapters-discord/src/ui/
├── mod.rs          — re-exports
├── format.rs       — format_duration(), format_duration_ms_diff()
├── progress.rs     — progress_bar(), progress_bar_paused()
└── colors.rs       — embed color constants
```

```rust
// ui/format.rs
/// Format milliseconds as M:SS. Returns "--:--" for unknown (≤0).
pub fn format_duration(ms: i32) -> String {
    if ms <= 0 { return "--:--".to_string(); }
    let secs = (ms as u32) / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

// ui/progress.rs
/// Unicode progress bar. filled=━, cursor=● (playing) or ⏸ (paused), empty=─
pub fn progress_bar(elapsed_ms: i64, total_ms: i32, width: usize, paused: bool) -> String {
    if total_ms <= 0 || elapsed_ms < 0 {
        return "─".repeat(width);
    }
    let ratio = (elapsed_ms as f64 / total_ms as f64).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width.saturating_sub(1));
    let cursor = if paused { '⏸' } else { '●' };
    format!(
        "{}{}{}",
        "━".repeat(filled),
        cursor,
        "─".repeat(width.saturating_sub(filled + 1))
    )
}

// ui/colors.rs
pub const COLOR_PLAYING: u32 = 0x1DB954;  // green
pub const COLOR_PAUSED:  u32 = 0xFAA61A;  // amber
pub const COLOR_QUEUE:   u32 = 0x5865F2;  // blurple
pub const COLOR_ERROR:   u32 = 0xED4245;  // red
```

Replace all inline implementations with imports from `crate::ui::*`.

---

### R2 — Embed color literals scattered

**Covered by R1.** Ensure every embed builder uses `ui::colors::COLOR_*`
constants instead of hardcoded hex literals. Search for `0x1D` and
`0xFA` and `0x58` to find all occurrences.

---

### R3 — `custom_id` built and parsed with raw string operations

**Problem.** Queue button custom_ids built as:
```rust
format!("queue_skip:{guild_id}:{user_id}")
```
and parsed as:
```rust
let parts: Vec<&str> = custom_id.split(':').collect();
let guild_id = parts[1];  // panics if malformed
```

This pattern is fragile: adding a field shifts all indices; a
`guild_id` containing `:` would split incorrectly (Discord snowflakes
don't contain `:`, but still).

**Fix.** A structured type with explicit serialization:

```rust
// crates/adapters-discord/src/ui/custom_id.rs

#[derive(Debug, Clone)]
pub enum QueueAction {
    PrevPage  { guild_id: u64, page: u32, user_id: u64 },
    NextPage  { guild_id: u64, page: u32, user_id: u64 },
    Pause     { guild_id: u64 },
    Skip      { guild_id: u64 },
    Shuffle   { guild_id: u64 },
    Clear     { guild_id: u64 },
}

impl QueueAction {
    pub fn to_custom_id(&self) -> String {
        match self {
            Self::PrevPage { guild_id, page, user_id } =>
                format!("qp|prev|{guild_id}|{page}|{user_id}"),
            Self::NextPage { guild_id, page, user_id } =>
                format!("qp|next|{guild_id}|{page}|{user_id}"),
            Self::Pause { guild_id }   => format!("qa|pause|{guild_id}"),
            Self::Skip { guild_id }    => format!("qa|skip|{guild_id}"),
            Self::Shuffle { guild_id } => format!("qa|shuffle|{guild_id}"),
            Self::Clear { guild_id }   => format!("qa|clear|{guild_id}"),
        }
    }

    pub fn from_custom_id(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('|').collect();
        match parts.as_slice() {
            ["qp", "prev", g, p, u] => Some(Self::PrevPage {
                guild_id: g.parse().ok()?,
                page:     p.parse().ok()?,
                user_id:  u.parse().ok()?,
            }),
            // ... etc
            _ => None,
        }
    }
}
```

`|` is used as delimiter (Discord snowflakes are decimal digits only —
no `|`). All button creation and interaction routing uses this type.

---

### R4 — Embed builders inline in command handlers

**Problem.** If the NP embed is built directly inside the `/nowplaying`
command handler and also inside the auto-update task, they will diverge.
Similar for the queue embed built in `/queue` and rebuilt after each
button press.

**Fix.** Each embed has a single builder function:

```rust
// crates/adapters-discord/src/ui/embeds.rs

pub fn build_np_embed(
    entry:      &QueueEntry,      // currently playing
    elapsed_ms: i64,              // actual elapsed time (pause-adjusted)
    paused:     bool,
    next_entry: Option<&QueueEntry>,
) -> serenity::builder::CreateEmbed { ... }

pub fn build_queue_embed(
    queue_meta: &[QueueEntry],
    page:       usize,
    per_page:   usize,
    paused:     bool,
) -> (serenity::builder::CreateEmbed, serenity::builder::CreateActionRow,
      serenity::builder::CreateActionRow) { ... }
```

Both the command handler and the auto-update task call the same builder.
Any visual change (new field, color tweak) only needs to happen once.

---

## Optimizations

### O1 — NP auto-update sends HTTP requests while paused

**File:** `lifecycle_worker.rs` — NP update task

The NP update task fires every 5 seconds regardless of pause state.
While paused, the progress bar doesn't advance — the edit sends the
same embed bytes every 5 seconds until the track is resumed.

**Fix.** Check pause state inside the update loop:

```rust
_ = tokio::time::sleep(interval) => {
    let state = state.lock().await;
    // Skip the edit entirely if paused — nothing has changed visually
    // (the ⏸ embed was already sent when /pause was called)
    if state.paused_at.is_some() {
        continue;
    }
    let embed = build_np_embed(...);
    // ... edit message
}
```

This halves Discord API calls during paused sessions.

---

## Self-Explore Items

### S1 — `/queue save` when queue is empty

**Verify:** What does `/queue save` return when `queue_meta` is empty?
If the implementation calls `PlaylistPort::create_playlist` and then
`add_track` zero times, it creates an empty playlist. That may be
intentional, or it may be better to guard:

```rust
if state.queue_meta.is_empty() {
    return ephemeral("Queue is empty — nothing to save.");
}
```

Decide and implement consistently.

---

### S2 — Discord component 15-minute interaction timeout

**Background.** Discord rejects button interactions on messages older
than 15 minutes with error code 10062 (`Unknown Interaction`). After
15 minutes of inactivity, pressing any queue button returns "This
interaction failed" to the user.

**Verify:** Does the interaction handler return a graceful error, or
does the unhandled 10062 bubble up as an unhandled error in logs?

**Minimum fix:** Catch the 10062 error code and respond with an
ephemeral message:

```rust
// In the interaction handler, after attempting to respond:
if let Err(serenity::Error::Http(http_err)) = result {
    if http_err.status_code() == Some(StatusCode::NOT_FOUND) {
        // Interaction has expired — user needs to run /queue again
        // Can't respond to an expired interaction, just log and move on
        tracing::debug!("Queue interaction expired (>15 min)");
        return Ok(());
    }
}
```

**Optional enhancement:** Disable all buttons after 14 minutes by
editing the message. This requires a timer per queue message — complex
for now. Acceptable to defer to a later pass.

---

### S3 — NP task orphan on crash/restart

**Document and accept.** On bot crash:
- The NP update task is killed with the process
- On restart, `GuildMusicState` is fresh — `nowplaying_message_id` is
  `None`, `np_update_cancel` is `None`
- The old NP message in Discord is left stale (no further edits)
- This is the correct behaviour per B3 decision (old messages left stale)

No fix needed. Add a code comment:

```rust
// On startup, np_update_cancel is always None.
// Any NP message from a previous session is left stale in Discord —
// intentional per UX decision B3. The bot posts a new NP message
// on the first TrackStarted event after restart.
```

---

### S4 — `TrackEnded` fires on skip AND natural track end — verify both paths

**File:** `crates/adapters-voice/src/track_event_handler.rs`

Songbird fires `TrackEvent::End` on both:
1. Natural end of track (audio finished)
2. `TrackHandle::stop()` (called by Songbird's queue when skipping)

Both paths run through the same `TrackEnded` event in the lifecycle
worker. Verify:

1. In the skip path, `track_play_started_at` and `total_paused_ms` are
   already set (they were set at `TrackStarted`), so `actual_play_ms`
   is computed correctly as a short duration.

2. The `completed` flag is `false` for a skipped-at-5s track:
   `5000ms / 240000ms = 0.02 < 0.80 threshold` ✓

3. After the skip, `queue_meta.remove(0)` is called and the next track's
   `TrackStarted` event fires, resetting `track_play_started_at` and
   `total_paused_ms` to their initial values.

4. If `queue_meta` becomes empty after the skip (no more tracks), no
   `TrackStarted` fires. Verify `track_play_started_at` is set to `None`
   when the queue empties, so a subsequent `/play` doesn't accumulate
   stale elapsed time.

If any of these are wrong, fix the handler.

---

## Verification Checklist

```bash
# Type safety verification:
cargo build --workspace 2>&1 | grep -E "^error|mismatched types|cannot convert"
cargo test --workspace

# Cast audit — find remaining as u32 on duration/millis:
grep -rn "as_millis.*as u32\|as_millis.*as i32\|as f32" \
    crates/adapters-voice/src/ crates/adapters-discord/src/
# Expected: zero results after fixes

# Utility deduplication — no inline format_duration or progress_bar:
grep -rn "fn format_duration\|fn progress_bar" \
    crates/adapters-discord/src/
# Expected: only in ui/format.rs and ui/progress.rs

# Custom_id raw splits — no .split(':') on interaction ids:
grep -rn "split(':')" crates/adapters-discord/src/
# Expected: zero results (only QueueAction::from_custom_id uses split)

# queue_meta sync — every TrackQueue modification has a matching queue_meta op:
grep -rn "TrackQueue\|\.enqueue\|\.dequeue\|\.skip()\|modify_queue" \
    crates/adapters-discord/src/ crates/adapters-voice/src/
# Manually verify each result has a corresponding queue_meta mutation nearby

# NULL duration display:
# Manually set duration_ms = NULL for one track in dev DB:
# UPDATE tracks SET duration_ms = NULL WHERE id = (SELECT id FROM tracks LIMIT 1);
# /play that track → /queue → verify "--:--" shown, no panic
# /nowplaying → verify "--:--" in progress bar area, no panic

# Bounds check on /skip:
# With 2 tracks in queue: /skip 99 → should return error, not crash
# With 1 track in queue:  /skip   → should return "Queue is empty after skip."

# Position 0 guard:
# /queue remove 1 → should fail with "Can't remove currently playing track"
# (since position 1 in display = index 0 in queue_meta)
```
