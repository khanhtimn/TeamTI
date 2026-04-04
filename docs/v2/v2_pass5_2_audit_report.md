# Pass 5.2 Audit Report

## Tool Output

### cargo tree (serenity/songbird/dashmap)

```
│   ├── dashmap v6.1.0
│   ├── serenity v0.12.5 (https://github.com/serenity-rs/serenity?branch=next#2fdbd065)
│   │   ├── dashmap v6.1.0 (*)
│   ├── songbird v0.5.0 (https://github.com/serenity-rs/songbird?branch=serenity-next#7d964c5a)
│   │   ├── dashmap v6.1.0 (*)
│   │   ├── serenity v0.12.5 (*) 
│   │   ├── serenity-voice-model v0.3.0
├── serenity v0.12.5 (*)
├── songbird v0.5.0 (*)
├── dashmap v6.1.0 (*)
```

✅ Single version of each dependency. No duplicates.

### Dependency version check

```
crates/adapters-voice/Cargo.toml:13:songbird = { workspace = true, features = [
crates/adapters-voice/Cargo.toml:15:    "serenity",
crates/adapters-voice/Cargo.toml:19:serenity = { workspace = true, features = [
crates/adapters-discord/Cargo.toml:13:songbird = { workspace = true, features = ["serenity"] }
crates/adapters-discord/Cargo.toml:14:serenity = { workspace = true, features = [
```

✅ All use `workspace = true`. No version strings alongside git refs.

### v1 command check

```
(no matches)
```

✅ No remnant v1 commands (ping, help).

### Error boundary check

```
(no matches)
```

✅ No rogue `pub enum Error` / `pub struct Error` in adapter crates.

### MutexGuard-across-await check

```
crates/adapters-voice/src/track_event_handler.rs:56:  let mut state = state_lock.lock().await;
crates/adapters-voice/src/track_event_handler.rs:89:  let mut state = state_lock.lock().await;
crates/adapters-voice/src/track_event_handler.rs:158: let mut state = state_lock.lock().await;
crates/adapters-voice/src/track_event_handler.rs:180: let state = state_lock.lock().await;
crates/adapters-voice/src/player.rs:47:               handler_lock.lock().await.queue().stop();
crates/adapters-voice/src/player.rs:96:               let mut handler = handler_lock.lock().await;
crates/adapters-discord/src/commands/play.rs:225:     let mut state = state_lock.lock().await;
crates/adapters-discord/src/commands/play.rs:248:     let state = state_lock.lock().await;
crates/adapters-discord/src/commands/play.rs:266:     let mut state = state_lock.lock().await;
crates/adapters-discord/src/commands/clear.rs:35:     let mut state = state_lock.lock().await;
crates/adapters-discord/src/commands/clear.rs:52:     handler_lock.lock().await.queue().stop();
crates/adapters-discord/src/commands/leave.rs:24:     let mut state = state_lock.lock().await;
```

Analysed below in R4.

### Defer check

```
crates/adapters-discord/src/commands/play.rs:101:   pub async fn run(
crates/adapters-discord/src/commands/rescan.rs:11:  pub async fn run(
crates/adapters-discord/src/commands/clear.rs:12:   pub async fn run(
crates/adapters-discord/src/commands/leave.rs:12:   pub async fn run(
```

All four commands verified to call `defer_ephemeral` as the first line in `run()`.

---

## R1 — Functional Correctness

### Flow 1: Happy path
**Status: PARTIAL**

The flow `/play → join → enqueue → track ends → next track plays → queue empty → auto-leave` is structurally correct. However:

> [!WARNING]
> **F1-A (BLOCK):** [play.rs:242](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-discord/src/commands/play.rs#L242) creates a **snapshot clone** of the `DashMap`:
> ```rust
> &Arc::new(guild_state_map.clone()),
> ```
> `guild_state_map` is `&GuildStateMap` (auto-deref from `&Arc<GuildStateMap>`). Calling `.clone()` on a `DashMap` clones all entries into a **new** `DashMap`. The `TrackEventHandler` receives this disposable copy. When `TrackEventHandler::act()` calls `self.state_map.get(&guild_id)`, it reads from the snapshot, not from the live state. **Result: meta_queue pops never reflect in the real state; auto-leave timers start on ghost state; now-playing updates write to a dead lock.**
>
> **Fix:** Change `play::run` to accept `guild_state_map: &Arc<GuildStateMap>` and pass `Arc::clone(guild_state_map)` to `enqueue_track`. This requires a signature change but zero logic change.

> [!NOTE]
> **F1-B (MEDIUM):** `/play` does not post the initial "Now Playing" embed when the first track starts. Only `TrackEventHandler` posts it — and only on track *end*, updating to the *next* track. The first track plays silently until it ends. Fix: post the "Now Playing" embed in `play::run` after a successful enqueue when `queue_pos == 1`.

### Flow 2: Queue during timer
**Status: PASS**

[play.rs:228](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-discord/src/commands/play.rs#L228) calls `state.cancel_auto_leave()` before enqueuing, which cancels any pending auto-leave `CancellationToken`. The timer task's `tokio::select!` biased branch picks up the cancellation. Correct.

### Flow 3: Channel move
**Status: PARTIAL**

> [!WARNING]
> **F3-A (HIGH):** When a user in Channel B calls `/play` while the bot is in Channel A, `join_channel()` calls `songbird.join(guild_id, channel_id)` which — per Songbird's `Call::join` — leaves Channel A first and joins Channel B. However, **Songbird's internal `Driver` is reset** during this reconnection. The builtin `TrackQueue` on the old `Driver` is lost. Meanwhile, `meta_queue` preserves the old metadata. **Result: meta_queue diverges from Songbird's actual queue after a channel move.**
>
> **Fix:** On channel move detection (i.e., `state.voice_channel_id.is_some() && state.voice_channel_id != Some(channel_id)`), clear `meta_queue` and re-enqueue only the new track. Or, accept queue loss on channel move and clear `meta_queue` in `play.rs` before the join call when moving channels.

### Flow 4: Hard stop
**Status: PASS**

`leave.rs` calls `state.cancel_auto_leave()`, `state.meta_queue.clear()`, resets `voice_channel_id` and `now_playing_msg`, then calls `leave_channel()` which does `queue().stop()` then `songbird.leave()`. No orphaned handlers (Songbird drops the `Driver` on leave, which drops all track handles and their event handlers).

### Flow 5: Bad file
**Status: PASS**

`enqueue_track()` at [player.rs:84](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-voice/src/player.rs#L84) checks `abs_path.exists()` before passing to Songbird. On failure, it returns `AppError::Voice { kind: FileNotFound }`, and `play.rs` rolls back `meta_queue.pop_back()`. The error is logged with structured fields and the user gets "Failed to queue the track."

For files that exist but are corrupted (decode error at playback time), Songbird's `QueueHandler` skips them and advances. Our `TrackEventHandler` (attached to `TrackEvent::Error`) logs the error and pops from `meta_queue`. Correct.

### Flow 6: Error recovery
**Status: PASS (conditional on F1-A fix)**

Songbird's builtin `QueueHandler` has a `while let Some(new) = inner.tracks.front()` loop that skips tracks that fail `play()`. Combined with our `TrackEvent::Error` handler for logging, this is robust. However, the current state divergence bug (F1-A) means the error handler operates on a dead state copy.

---

## R2 — Architectural Consistency

### R2.1 Error types
**Status: PASS**

All voice errors use `AppError::Voice { kind: VoiceErrorKind, detail }`. No separate error enums cross crate boundaries. `VoiceErrorKind` is defined in `application/src/error.rs` with 5 variants: `NotInitialized`, `JoinFailed`, `NotInChannel`, `FileNotFound`, `DecodeError`.

### R2.2 Structured logging
**Status: PARTIAL**

> **F-R2.2 (LOW):** Some `warn!`/`error!` calls in `track_event_handler.rs` use `error.kind = "voice.track_decode_error"` as a string literal rather than `error.kind = e.kind_str()` on an actual `AppError`. Technically correct (it's a Songbird event, not an `AppError`), but inconsistent with the pattern used elsewhere. Minor.
>
> The `autocomplete` function in `play.rs:95` logs `error = %e` but lacks an `operation` field. Minor.

### R2.3 Dependency versions
**Status: PASS**

Workspace manifest declares `serenity` and `songbird` as `git` dependencies with branch refs. All crate-level Cargo.toml files use `workspace = true`. `cargo tree` confirms single versions. No version strings appear alongside git refs.

### R2.4 Cancellation
**Status: PARTIAL**

> **F-R2.4 (MEDIUM):** The auto-leave timer task in `track_event_handler.rs:82` creates its own `CancellationToken` and is cancellable via `cancel_auto_leave()`. However, this task does **not** respect the application-level `CancellationToken` from `apps/bot/src/main.rs`. If the bot is shutting down (Ctrl+C → `token.cancel()`), the auto-leave timer could still fire and attempt an HTTP leave call during shutdown. Fix: pass the application-level `CancellationToken` into `TrackEventHandler` and add it as a third `select!` branch.

---

## R3 — Queue Architecture

### Current approach

**Hybrid.** Songbird's `builtin-queue` feature is enabled. `enqueue_track()` calls `handler.enqueue_input()` which adds to Songbird's `TrackQueue`. A parallel `VecDeque<QueuedTrack>` (`meta_queue`) holds domain metadata. `TrackEventHandler` attached to `TrackEvent::End` and `TrackEvent::Error` pops from `meta_queue` and handles Discord embeds + auto-leave.

### Comparison table

| Criterion | Custom VecDeque | Songbird Built-in | **Hybrid (current)** |
|---|---|---|---|
| Pass 5 requirements met | ✅ (with bugs) | ❌ (no metadata) | ✅ |
| Pass 6 skip/pause/resume | ❌ manual | ✅ `queue().skip()` | ✅ delegated |
| Channel move safety | ❌ intentional_stop hack | ⚠️ queue lost on rejoin | ⚠️ same + meta diverge |
| TrackEvent handling | Full manual | Internal + hooks | Internal + metadata sync |
| Code complexity | High | Low | Medium |
| Risk of race conditions | High (try_advance_queue loop) | Low | **Medium (meta_queue sync)** |

### Recommendation

The **hybrid approach is correct** for this project's requirements. The key risk (meta_queue/Songbird divergence) is manageable with two fixes:
1. Fix the state_map clone bug (F1-A) — this is the only BLOCK item
2. Handle channel-move queue loss explicitly (F3-A)

The hybrid approach is also the best foundation for Pass 6: `queue().skip()`, `queue().pause()`, `queue().resume()` all delegate to Songbird while our handler keeps the metadata display in sync.

---

## R4 — Thread Safety

All `GuildMusicState` mutations hold the `tokio::sync::Mutex` via `.lock().await`. Each critical section is analysed:

| Location | Guard held | `.await` while held? | Verdict |
|---|---|---|---|
| track_event_handler.rs:56-60 | `state` | No (pop_front is sync) | ✅ |
| track_event_handler.rs:62-80 | `state` | Dropped at 76/79 before `.await` | ✅ |
| track_event_handler.rs:89 | spawned task | Dropped at 92 before leave_channel | ✅ |
| track_event_handler.rs:158 | `state` | Acquired, set, implicit drop on scope exit | ✅ |
| track_event_handler.rs:180 | `state` | Acquired, read `len()`, immediately dropped | ✅ |
| play.rs:225-232 | `state` | No `.await` in block | ✅ |
| play.rs:248-249 | `state` | Dropped immediately via block | ✅ |
| play.rs:266-267 | `state` | Dropped immediately via block | ✅ |
| clear.rs:35-48 | `state` | No `.await` before Songbird lock | ✅ |
| leave.rs:24-42 | `state` | No `.await` in critical section | ✅ |
| player.rs:96-97 | `handler` (`Call` mutex) | `enqueue_input().await` **is called while holding the Call lock** | ⚠️ |

> **F-R4-A (MEDIUM):** [player.rs:96-97](file:///Users/khanhtimn/Documents/project/teamti/crates/adapters-voice/src/player.rs#L96-L97) holds the `Call` Mutex guard across `handler.enqueue_input(source.into()).await`. This is Songbird's `parking_lot::Mutex` (sync, not tokio), but `enqueue_input` is an `async fn` that may do I/O to read `AuxMetadata` for preloading. In practice this should be fast for local files, but it blocks the Call mutex for all other guilds sharing the same `handler_lock`. Not a deadlock risk but a latency risk. Can be deferred.

**TOCTOU analysis:** The `meta_queue.push_back()` in play.rs happens before `enqueue_track()`, and rollback happens in the error path. If two concurrent `/play` commands race, both pushes happen atomically (each holds the lock in its own block), and both enqueues happen sequentially (Songbird serializes via the `Call` lock). Order is consistent.

---

## R5 — User Experience

| Command | Defers? | Error messages actionable? | Now-playing updated? |
|---|---|---|---|
| `/play` | ✅ `defer_ephemeral` | ✅ "You must be in a voice channel" / "Track not found" | ⚠️ Not posted on first track |
| `/clear` | ✅ `defer_ephemeral` | ✅ "No active playback" / "Queue already empty" | ❌ Not updated after clear |
| `/leave` | ✅ `defer_ephemeral` | ✅ "Not in a voice channel" / "Failed to leave" | N/A (embed becomes stale) |
| `/rescan` | ✅ `defer_ephemeral` | ✅ | N/A |

> **F-R5-A (MEDIUM):** After `/clear`, the now-playing embed is not updated to show "Queue Ended". The embed continues to show the last track as if it were still playing. Fix: post a "Queue Ended" embed in `clear.rs` after clearing.

---

## Summary

### BLOCK items
| ID | Requirement | Issue | Fix |
|---|---|---|---|
| F1-A | R1 Flow 1 | `play.rs:242` clones `DashMap` into a snapshot; `TrackEventHandler` uses dead state | Change `play::run` to accept `&Arc<GuildStateMap>` and pass `Arc::clone()` |

### HIGH items
| ID | Requirement | Issue | Fix |
|---|---|---|---|
| F3-A | R1 Flow 3 | Channel move loses Songbird queue but preserves `meta_queue` — divergence | Detect channel move in `/play`, clear `meta_queue` before rejoin |

### MEDIUM items
| ID | Issue |
|---|---|
| F1-B | No "Now Playing" embed posted on first track |
| F-R2.4 | Auto-leave timer doesn't respect app-level shutdown `CancellationToken` |
| F-R4-A | `Call` mutex held across `enqueue_input().await` |
| F-R5-A | `/clear` doesn't update now-playing embed |

### LOW items
| ID | Issue |
|---|---|
| F-R2.2 | Minor inconsistencies in structured logging fields |

### Estimated effort to reach BLOCK-clear state

**1 finding, ~15 lines changed, 2 files affected** (`play.rs` signature + call site, `handler.rs` to pass `Arc` instead of deref).

### Estimated effort to reach HIGH-clear state

**+1 finding, ~20 lines added** in `play.rs` (channel move detection + `meta_queue` clear).
