# TeamTI v3 — Pass 5 Implementation Prompt
## Queue, Skip, Pause/Resume, Now Playing & Discord UX

> Attach alongside: `teamti_v3_pass5_design.md`
> Also attach: `state.rs`, `player.rs`, `track_event_handler.rs`,
> `lifecycle_worker.rs`, and all existing command files.
>
> The design spec is authoritative for all type names, embed layouts,
> button custom_id formats, and state field definitions.
> This prompt describes goals, UX philosophy, and the acceptance bar.

---

## What This Pass Builds

TeamTI already plays music. Pass 5 makes it feel like a real music
platform to use in Discord. When this pass is done, every user in a
voice session has full awareness and control:

- They can see exactly what is queued, who added each track, and
  roughly how long until each track plays.
- They can pause and resume without losing their place or corrupting
  the listen history.
- They can skip to any track by name or position.
- They see a Now Playing embed appear automatically when each new track
  starts — with a live progress bar that advances every 5 seconds.
- The queue embed has action buttons directly — no memorising slash
  commands for common actions.

This pass is primarily about **Discord UX surface**, not audio engine
work. The Songbird audio engine is already correct. The work is in
building rich embeds, managing auto-update tasks, and keeping in-memory
queue metadata in sync with Songbird's internal state.

---

## UX Philosophy for This Pass

This pass explicitly includes improving the **ergonomics of Discord
modal and embed display** across the whole bot. Apply these principles
to every embed you build or touch in this pass:

**1. State is immediately visible.**
A user who opens `/queue` or `/nowplaying` should know at a glance:
what is playing (▶), whether it is paused (⏸), how far through it is
(progress bar), who added it, and what comes next. No hunting for
information.

**2. Color signals state.**
- Green `0x1DB954`: playing
- Amber `0xFAA61A`: paused
- Blurple `0x5865F2`: queue view (neutral)
- Red `0xED4245`: error states (use sparingly, only for actual errors)

**3. Buttons are context-aware.**
A button that does nothing in the current state is disabled — not
hidden. The user sees it exists but understands it is unavailable
now. Hiding buttons is confusing; disabling them with a visual cue is
clear. Example: Skip button is disabled when only one track is queued.

**4. Ephemeral for personal feedback, public for shared state.**
- Action confirmations (⏭ Skipped, ⏸ Paused) → ephemeral
- Queue view → public (the whole channel can see and interact)
- NP auto-post → public (announces the new track to everyone)
- Error messages → always ephemeral

**5. Radio tracks are visually distinct.**
A 🎲 prefix on radio-added tracks sets expectations — users know
these came from the recommendation engine, not a human's choice.

**6. Consistency with Pass 3 pagination.**
Queue pagination uses the same button layout (◀ Prev / Page N/M /
Next ▶) as the playlist and favourites views. Users should not need
to learn two different navigation schemes.

---

## Critical Implementation Details

### Queue metadata and Songbird stay in sync

`GuildMusicState.queue_meta: Vec<QueueEntry>` is the in-memory
metadata parallel to Songbird's `TrackQueue`. These MUST be modified
together in every operation. If you call `TrackQueue::dequeue(2)`,
you must also call `queue_meta.remove(2)` in the same lock scope.
A divergence between `queue_meta` and the actual Songbird queue will
cause the wrong track names/positions to be shown in the queue embed.

Audit every existing location where Songbird's queue is modified
(enqueue, skip, playlist play) and ensure `queue_meta` is updated
there too.

### Pause duration is subtracted from play_duration_ms

This is the fix for the listen event accuracy problem (Pass 5 design
spec Option A). The formula is:

```
actual_play_ms = elapsed_since_track_start - total_paused_ms
```

Both `total_paused_ms` and `track_play_started_at` must be reset on
every `TrackStarted` event. Do not carry pause state between tracks.

### NP auto-update uses `CancellationToken`, not a raw `JoinHandle`

Use `tokio_util::sync::CancellationToken`. When a new track starts:
1. Cancel the old token (if any).
2. Create a new token.
3. Spawn the update task with the new token.

Do NOT use `JoinHandle::abort()` — it causes a panic-on-drop in some
tokio versions. The cancellation token pattern is clean and explicit.

### `/leave` must clean up all new state

The `/leave` command (and the auto-leave-on-empty logic in
`track_event_handler.rs`) must cancel the NP update token and clear
all the new `GuildMusicState` fields. Missing this means the update
task continues running against a deleted message, logging spurious
errors every 5 seconds.

### Queue embed action buttons are NOT user-gated for actions

Unlike playlist pagination (which gates Prev/Next to the invoking
user), queue action buttons (pause, skip, shuffle, clear) accept
input from any user. Only the Prev/Next pagination buttons encode
the invoking user in the custom_id for session ownership.

---

## What This Pass Does NOT Do

- No vote-skip (any user can skip freely per Q8).
- No DJ role permissions.
- No queue persistence across bot restarts (in-memory only).
- No "history" of previously played tracks (covered by /history from Pass 3).
- No per-user queue view (the queue is shared and public).
- No `/stop` command (removed per design decision).
- No NP message deletion on track change (left stale per B3).
- No NP auto-update cancellation on pause — the edit continues but
  shows ⏸ state with the static progress bar position.

---

## Definition of Done

Pass 5 is complete when:

1. `cargo build --workspace` produces zero errors and zero warnings.
2. `cargo test --workspace` passes.
3. `/queue` shows all queued tracks with correct positions, ▶ marker,
   and radio 🎲 labels. Pagination and all 4 action buttons work.
4. `/pause` changes the NP embed to amber ⏸, progress bar freezes.
5. `/resume` changes it back to green ▶, progress bar continues.
6. `/skip` alone advances one track. `/skip 3` jumps to position 3
   and discards positions 1-2. Autocomplete shows `N. Title — Artist`.
7. A new NP embed is auto-posted every time a new track starts,
   in the same channel as the last `/play` command.
8. The NP embed progress bar visibly advances every 5 seconds.
9. `/leave` cancels the NP task — no further message edits after leave.
10. After pausing for 60 seconds and resuming, skipping at 10 seconds:
    the DB records `play_duration_ms ≈ 10000` (not `70000`).
    The listen event shows `completed = false`.
11. `/queue save "My Mix"` creates a playlist with all current queue
    tracks. Running it again with the same name returns the AlreadyExists
    error with a helpful suggestion.
12. All error messages are ephemeral and human-readable. No raw
    Rust error strings are ever shown in Discord.
