# TeamTI v2 — Pass 5.1 Prompt
## Reflection, Correctness Audit & Performance Review (Discord Integration)

> Review-only pass. No new features. Every change must cite a finding below.
> Read the full Pass 5 implementation before starting the checklist.
> Apply Critical and High fixes directly. Document Medium/Low if structurally
> out of scope.

---

### Objective

Pass 5 introduced two new crates (`adapters-voice`, `adapters-discord`),
a per-guild state machine (`GuildMusicState`), Songbird audio playback,
and three slash commands. This pass audits four specific risk categories:

1. **Version/API alignment** — Pass 5 specified published crate versions for
   Serenity and Songbird, but v1 pinned git development branches. Any API
   mismatch between the published crate and the git HEAD will cause
   compile failures with confusing error messages.

2. **Race conditions** in the shared mutable `GuildMusicState` — particularly
   the interaction between intentional track stops and the `TrackEndHandler`

3. **Compile failures** from incorrect type conversions and missing trait methods

4. **Error architecture violations** — `VoiceError` is a separate enum that
   bypasses the unified `AppError` hierarchy established in Pass 4.5.

---

### File Inventory

```
crates/adapters-voice/src/state.rs
crates/adapters-voice/src/player.rs
crates/adapters-voice/src/track_end_handler.rs
crates/adapters-voice/src/error.rs
crates/adapters-voice/Cargo.toml
crates/adapters-discord/src/commands/play.rs
crates/adapters-discord/src/commands/clear.rs
crates/adapters-discord/src/commands/leave.rs
crates/adapters-discord/src/commands/rescan.rs
crates/adapters-discord/src/handler.rs
apps/bot/src/main.rs
crates/application/src/error.rs          ← must be updated in this pass
```

Also run before starting:
```bash
# Verify v1 pinned git refs for serenity and songbird
grep -A3 "serenity\|songbird" Cargo.toml
grep -A3 "serenity\|songbird" crates/adapters-voice/Cargo.toml
grep -A3 "serenity\|songbird" crates/adapters-discord/Cargo.toml

# Verify no v1 commands remain
grep -rn "\"ping\"\|CreateCommand.*ping\|fn ping" --include="*.rs" .
```
Expected for last command: **empty**.

---

### Audit Checklist

---

#### SECTION A — Critical: Version Alignment & Race Conditions

**A1. Serenity and Songbird must use the same git references as v1**

Pass 5 specifies:
```toml
serenity = { version = "0.12", ... }
songbird = { version = "0.4", ... }
```

**This is wrong.** v1 pinned both libraries to git development branches.
The git HEAD API for Serenity and Songbird differs from the published crates
in ways that cause silent behavioral differences or hard compile failures.

Check: open the root `Cargo.toml` (workspace manifest) and find the existing
`serenity` and `songbird` entries established in v1. They will be in one
of these forms:
```toml
serenity = { git = "https://github.com/serenity-rs/serenity",
             branch = "current", ... }
songbird = { git = "https://github.com/serenity-rs/songbird",
             branch = "current", ... }
```

**Fix:** copy the exact git reference (including `git`, `branch` or `rev`,
and `features`) from the workspace `Cargo.toml` into both
`crates/adapters-voice/Cargo.toml` and `crates/adapters-discord/Cargo.toml`.
Do NOT introduce a version string alongside a git reference — Cargo will
reject the combination.

If serenity and songbird are declared in `[workspace.dependencies]`, use:
```toml
# crates/adapters-voice/Cargo.toml
serenity = { workspace = true }
songbird = { workspace = true }
```

This is the only safe approach — it guarantees a single resolved version
across all crates.

**API surface differences to audit after aligning versions:**

The following APIs changed between the last published crate and git HEAD.
After version alignment, verify each one compiles correctly:

| API | Published (0.12) | Git HEAD (may differ) |
|---|---|---|
| `Interaction::Autocomplete` | `Interaction::Autocomplete(AutocompleteInteraction)` | May be `Interaction::Autocomplete(CommandInteraction)` with autocomplete flag |
| `GuildId::new(u64)` | Stable | Check — older git HEAD uses `GuildId(u64)` tuple struct |
| `CreateCommand::new(name)` | Stable in 0.12 | May be `CreateApplicationCommand::name(...)` in older branch |
| `Command::set_global_commands` | `serenity::model::application::Command` | May be under `serenity::model::interactions` |
| `interaction.defer_ephemeral()` | Returns `Result<()>` | Check if method exists or must use `create_interaction_response` directly |
| `EditInteractionResponse::new()` | Stable | May be `EditInteractionResponse::default()` |
| `interaction.edit_response()` | Stable | May require `.http` call chain |

For each API that differs: update Pass 5 code to match the git HEAD API.
Do not patch the library — patch the call sites.

Severity: **Critical** — wrong version specifiers compile against a different
API surface; all voice and Discord code may fail to compile or behave
unexpectedly at runtime.

---

**A2. `handle.stop()` fires `TrackEvent::End`, causing phantom queue advance**

This is the most dangerous logic bug in Pass 5.

**Scenario — channel move:**
In `commands/play.rs`, when the bot is in Channel A and the user is in Channel B:

```rust
if let Some(current) = state.current_track.take() {
    current.handle.stop().ok();          // ← fires TrackEvent::End
    state.queue.push_front(current.track);
}
```

Calling `TrackHandle::stop()` fires `TrackEvent::End`. The `TrackEndHandler`
registered on that handle wakes up and calls `pop_front()` on the queue —
which now contains the re-inserted current track. Simultaneously, the `/play`
command calls `join_channel()` then `play_track()` for the same track.

Two concurrent `play_track()` calls for the same guild ID. Depending on
Songbird's internal locking, this causes either two simultaneous audio streams
or a panic.

**Fix: add `intentional_stop: bool` to `GuildMusicState`.**

```rust
// state.rs
pub struct GuildMusicState {
    // ... existing fields ...
    pub intentional_stop: bool,
}

impl GuildMusicState {
    pub fn new() -> Self {
        Self {
            intentional_stop: false,
            // ...
        }
    }
}
```

Set it before any intentional stop:

```rust
// commands/play.rs — channel move
if let Some(current) = state.current_track.take() {
    state.intentional_stop = true;       // suppress TrackEndHandler
    current.handle.stop().ok();
    state.queue.push_front(current.track);
}

// commands/leave.rs — hard stop
if let Some(current) = state.current_track.take() {
    state.intentional_stop = true;
    current.handle.stop().ok();
}
```

Check and consume the flag at the start of `TrackEndHandler::act()`:

```rust
let mut state = state_lock.lock().await;
if state.intentional_stop {
    state.intentional_stop = false;
    return None;   // do not advance queue
}
state.current_track = None;
// ... rest of handler ...
```

Severity: **Critical** — phantom double-play on every channel move;
potential Songbird panic under concurrent lock contention.

---

**A3. `TrackEvent::Error` not handled — decoding errors silently stall the queue**

If Songbird encounters an audio decode error mid-stream (corrupted file,
NAS disconnection, unsupported codec variant), it fires `TrackEvent::Error`,
NOT `TrackEvent::End`. The current `TrackEndHandler` only registers for
`TrackEvent::End`. A decode error silently stops the track without advancing
the queue. The bot appears stuck in the channel playing nothing.

**Fix:** register the same handler for both events. `TrackEndHandler` must
derive or implement `Clone`:

```rust
// After play_track() returns a handle:
handle.add_event(
    Event::Track(songbird::TrackEvent::End),
    handler.clone(),
).ok();

handle.add_event(
    Event::Track(songbird::TrackEvent::Error),
    handler,   // move last one, clone first
).ok();
```

In `TrackEndHandler::act()`, check the event context to log correctly:

```rust
// Detect error vs normal end
let is_error = matches!(event_ctx,
    EventContext::Track(ts) if matches!(ts.playing, PlayMode::Errored(_))
);

if is_error {
    tracing::warn!(
        guild_id   = %self.guild_id,
        error.kind = "voice.track_decode_error",
        operation  = "track_end_handler.error",
        "track ended with decode error — advancing queue"
    );
}
```

Note: verify `PlayMode` and `EventContext::Track` exist under these names in
the git HEAD version of Songbird. API paths may differ.

Severity: **Critical** — silent queue stall on any file decode error;
user cannot recover without `/leave`.

---

#### SECTION B — High: Error Architecture & Correctness

**B1. `VoiceError` violates the unified `AppError` architecture from Pass 4.5**

Pass 4.5 established a single `AppError` type for the entire workspace with
a `kind_str()` method, a `Retryable` trait impl, and typed kind enums for
each external domain. Pass 5 introduced `VoiceError` as a separate enum in
`adapters-voice/src/error.rs`, bypassing this architecture entirely.

Consequences:
- `VoiceError` has no `kind_str()` — cannot be logged with the structured
  `error.kind` field established in Pass 4.5
- `VoiceError` has no `Retryable` impl — the error handling path in
  command handlers cannot use the retryability signal
- `VoiceError` is a different type from `AppError` — command handlers that
  return `Result<_, AppError>` must manually convert or match on `VoiceError`,
  losing the `?` operator ergonomics

**Fix:** integrate `VoiceError` into `AppError` using the same pattern as
`AcoustIdErrorKind` from Pass 4.5.

Add to `application/src/error.rs`:

```rust
// New kind enum — replaces VoiceError variants
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum VoiceErrorKind {
    #[error("Songbird not initialized")]   NotInitialized,
    #[error("failed to join channel")]     JoinFailed,
    #[error("not in a voice channel")]     NotInChannel,
    #[error("audio file not found")]       FileNotFound,
    #[error("track decode error")]         DecodeError,
}

// New AppError variant
#[error("voice error: {kind} — {detail}")]
Voice {
    kind:   VoiceErrorKind,
    detail: String,
},
```

Add to `AppError::kind_str()`:
```rust
AppError::Voice { kind, .. } => match kind {
    VoiceErrorKind::NotInitialized => "voice.not_initialized",
    VoiceErrorKind::JoinFailed     => "voice.join_failed",
    VoiceErrorKind::NotInChannel   => "voice.not_in_channel",
    VoiceErrorKind::FileNotFound   => "voice.file_not_found",
    VoiceErrorKind::DecodeError    => "voice.decode_error",
},
```

Add to `Retryable for AppError`:
```rust
AppError::Voice { kind, .. } => matches!(
    kind,
    VoiceErrorKind::JoinFailed   // retryable: transient connection issue
),
```

**Remove** `crates/adapters-voice/src/error.rs` entirely.

Update all sites in `adapters-voice/` that construct or match `VoiceError`
to use `AppError::Voice { kind: VoiceErrorKind::..., detail: "..." }` instead.

Add to `adapters-voice/Cargo.toml`:
```toml
application = { path = "../application" }
```
(if not already present)

Severity: **High** — architectural violation of Pass 4.5; `error.kind` log
field is missing from all voice errors; `?` operator breaks at
`AppError` / `VoiceError` boundaries.

---

**B2. Log events in voice/discord crates don't follow Pass 4.5 field schema**

Pass 4.5 mandated a structured log field schema:

| Field | Rule |
|---|---|
| `error = %e` | Required on every `warn!` and `error!` call |
| `error.kind = e.kind_str()` | Required on every `warn!` and `error!` call |
| `operation = "crate.action"` | Required on every log event |
| No dynamic data in message string | Static string only |

Check all `warn!` and `error!` calls in `adapters-voice/` and
`adapters-discord/`. Common violations to look for:

```rust
// WRONG — dynamic data in message
warn!("failed to join channel {channel_id}: {e}");

// CORRECT — structured fields
warn!(
    channel_id = %channel_id,
    error      = %e,
    error.kind = e.kind_str(),
    operation  = "player.join_channel",
    "failed to join voice channel"
);
```

After B1 is fixed (VoiceError → AppError::Voice), `e.kind_str()` is available
on all voice errors via the existing `kind_str()` impl.

Run a targeted audit:
```bash
grep -n "warn!\|error!" \
    crates/adapters-voice/src/*.rs \
    crates/adapters-discord/src/commands/*.rs \
    crates/adapters-discord/src/handler.rs
```

For each result: verify `error.kind` and `operation` are present as
structured fields. Fix any that are missing.

Severity: **High** — log events without `error.kind` are invisible to
`error.kind = "voice.*"` alert rules established in Pass 4.5.

---

**B3. `domain::Track::default()` — compile failure on free-text search path**

In `commands/play.rs`, the free-text search fallback:

```rust
Some(domain::Track {
    id:             summary.id,
    title:          summary.title,
    artist_display: summary.artist_display,
    ..Default::default()
})
```

`domain::Track` does not implement `Default` — it has required fields
(`id: Uuid`, `blob_location: String`) with no sensible default values.
This will not compile.

**Fix:** implement `From<TrackSummary> for QueuedTrack` directly in
`adapters-voice/src/state.rs`, eliminating the `domain::Track` intermediate:

```rust
impl From<domain::TrackSummary> for QueuedTrack {
    fn from(s: domain::TrackSummary) -> Self {
        QueuedTrack {
            track_id:      s.id,
            title:         s.title,
            artist:        s.artist_display.unwrap_or_default(),
            album:         None,
            duration_ms:   s.duration_ms,
            blob_location: s.blob_location,
        }
    }
}
```

In `commands/play.rs`, replace the `domain::Track` conversion:
```rust
// free-text search result → QueuedTrack directly
let queued = QueuedTrack::from(summary);
```

Severity: **High** — compile failure; free-text `/play` path is broken.

---

**B4. `builtin-queue` Songbird feature conflicts with custom queue**

`adapters-voice/Cargo.toml` declares:
```toml
songbird = { ..., features = ["builtin-queue"] }
```

With `builtin-queue` enabled, `Call::play()` routes tracks through Songbird's
internal `TrackQueue` rather than playing immediately. The custom `GuildMusicState`
`VecDeque` and `TrackEndHandler` assume full manual control of when tracks start.
Using both creates unpredictable ordering — Songbird's internal queue may
auto-advance tracks independently.

**Fix:** verify the workspace-level Songbird git reference does not include
`builtin-queue` in its feature set (it likely does not, as this is an
opt-in feature). Then in `adapters-voice/Cargo.toml`, do NOT add it:

```toml
# If using workspace reference:
songbird = { workspace = true }

# If declaring directly (should not be needed after A1 fix):
songbird = { git = "...", branch = "...",
             features = ["serenity", "driver", "rustls"] }
             # ← NO "builtin-queue"
```

Severity: **High** — Songbird internal queue conflicts with custom queue;
tracks may play out of order or skip entries.

---

**B5. `play_track` failure leaves queue permanently stuck**

After `play_track` fails (file missing, Songbird error), `current_track`
stays `None` and any remaining queue items are never advanced. `is_idle()`
returns `false` (queue is non-empty), so subsequent `/play` calls just
append without triggering playback. The queue is dead until `/leave`.

**Fix:** extract queue advancement into a shared helper `try_advance_queue`
used by both the `/play` command and `TrackEndHandler`. On `play_track`
failure, skip the bad track, log with structured fields, and loop to the
next item:

```rust
// In application layer or adapters-voice — shared queue advance logic
async fn try_advance_queue(
    ctx:             &Context,
    guild_id:        GuildId,
    state_lock:      &Arc<Mutex<GuildMusicState>>,
    media_root:      &PathBuf,
    auto_leave_secs: u64,
) {
    loop {
        let next = state_lock.lock().await.queue.pop_front();
        match next {
            None => {
                start_auto_leave(ctx, guild_id, state_lock, auto_leave_secs).await;
                return;
            }
            Some(track) => match play_track(ctx, guild_id, &track, media_root).await {
                Ok(handle) => {
                    register_track_end_handler(&handle, ctx, guild_id,
                                               media_root, auto_leave_secs);
                    state_lock.lock().await.current_track =
                        Some(CurrentTrack { track, handle });
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        guild_id   = %guild_id,
                        track_id   = %track.track_id,
                        error      = %e,
                        error.kind = e.kind_str(),
                        operation  = "queue.advance_skip",
                        "skipping unplayable track, advancing queue"
                    );
                    // continue loop — try next track
                }
            },
        }
    }
}
```

Replace the inline dequeue-and-play logic in both `commands/play.rs` and
`track_end_handler.rs` with calls to this helper. This also eliminates
the code duplication between the two sites.

Severity: **High** — permanently stuck queue after any single bad file.

---

**B6. `/leave` gives confusing error when bot is not in a voice channel**

`leave_channel()` is called unconditionally. If the bot is not connected,
`manager.leave(guild_id)` returns `Err(JoinError::NoCall)` (or the git HEAD
equivalent). The command responds with "Left with error: ..." — confusing
when the intent was just to confirm disconnection.

**Fix:** check `state.voice_channel_id` before attempting to leave:

```rust
let is_connected = state_map
    .get(&guild_id)
    .map(|s| s.blocking_lock().voice_channel_id.is_some())
    .unwrap_or(false);

if !is_connected {
    let _ = interaction.edit_response(ctx,
        EditInteractionResponse::new()
            .content("I'm not currently in a voice channel."))
        .await;
    return;
}
```

Severity: **High** — confusing user-facing error on a valid, harmless invocation.

---

**B7. `TrackSearchPort::autocomplete` — verify method name and signature**

The `/play` autocomplete handler calls:
```rust
search_port.autocomplete(partial, 25).await
```

Check in `application/src/ports/repository.rs`:
1. Does a method named `autocomplete` exist on `TrackSearchPort`?
2. Is the signature `async fn autocomplete(&self, prefix: &str, limit: usize) -> Result<Vec<TrackSummary>, AppError>`?
3. Does `TrackRepositoryImpl` implement it?

If the method has a different name (e.g. `search_prefix`, `suggest`,
`search_autocomplete`), update the call site in `commands/play.rs` to match.
Do NOT rename the trait method — that breaks all existing implementations.

Severity: **High** — compile failure if name or signature mismatches.

---

#### SECTION C — Medium: Robustness & UX

**C1. Album never shown in now-playing embed**

`QueuedTrack.album` is always `None` because both `From<&domain::Track>` and
`From<TrackSummary>` set `album: None`. The embed conditionally shows the
album field, so it is always missing.

**Fix for Pass 5.1:** fetch album title in `/play` command after resolving
the track, before building `QueuedTrack`:

```rust
let album_title = if let Some(album_id) = track.album_id {
    album_repo.find_by_id(album_id).await
        .ok().flatten()
        .map(|a| a.title)
} else {
    None
};

let queued = QueuedTrack {
    album: album_title,
    ..QueuedTrack::from(&track)   // after B3 fix: from TrackSummary
};
```

This adds one DB round trip per `/play` invocation — acceptable.

Severity: **Medium** — no album in now-playing embed for fully enriched tracks.

---

**C2. Global command registration has ~1 hour propagation delay**

Pass 5 registers commands globally via `Command::set_global_commands`.
Global registration takes up to 1 hour to propagate. During development and
initial testing, commands are invisible for up to an hour after deployment.

**Fix:** support guild-scoped registration via `DISCORD_TEST_GUILD_ID` env var:

```rust
let test_guild = std::env::var("DISCORD_TEST_GUILD_ID")
    .ok()
    .and_then(|s| s.parse::<u64>().ok())
    .map(GuildId::from);  // use GuildId::from or GuildId::new per git HEAD API

if let Some(guild_id) = test_guild {
    guild_id.set_commands(&ctx, commands).await.ok();
    tracing::info!(
        guild_id  = %guild_id,
        operation = "discord.guild_commands_registered",
        "registered guild-scoped commands (test mode)"
    );
} else {
    Command::set_global_commands(&ctx, commands).await.ok();
    tracing::info!(
        operation = "discord.global_commands_registered",
        "registered global slash commands"
    );
}
```

Add to `.env`:
```
# Optional: for testing only. Remove for production.
# DISCORD_TEST_GUILD_ID=your_guild_id
```

Severity: **Medium** — development friction only; no production impact.

---

**C3. `to_guild_cached()` returns None on reconnect**

`guild_id.to_guild_cached(ctx)` reads Serenity's in-memory guild cache.
The cache may be empty immediately after a reconnect before `GUILD_CREATE`
events are replayed. The `/play` command fails with "Unable to read guild
state" in this transient window.

Update the error message to be actionable:
```rust
None => {
    let _ = interaction.edit_response(ctx,
        EditInteractionResponse::new()
            .content("Guild state unavailable — this is usually transient \
                      after a reconnect. Please try again in a moment."))
        .await;
    return;
}
```

Severity: **Medium** — transient failure after bot reconnect.

---

**C4. Stale now-playing message after bot restart**

`GuildMusicState` is in-memory. After a restart, `now_playing_msg` is `None`.
The bot posts a new now-playing message, leaving the previous one unedited in
the chat history indefinitely.

Add a TODO comment marking this for Pass 6:
```rust
// track_end_handler.rs — post_now_playing()
// TODO(pass6): on startup, read persisted (channel_id, message_id) from DB
// and attempt to edit stale now-playing messages to show "Bot restarted."
// Requires a new migration to store these values.
```

Severity: **Low** — cosmetic; stale embeds accumulate across restarts.

---

#### SECTION D — Dependency & Config Audit

**D1. Workspace `[workspace.dependencies]` must declare Serenity and Songbird**

After the A1 fix, Serenity and Songbird must be declared exactly once in the
workspace manifest. Verify:

```toml
# Root Cargo.toml [workspace.dependencies]
serenity = { git = "https://github.com/serenity-rs/serenity",
             branch = "...",   # exact branch/rev from v1
             default-features = false,
             features = [...] }

songbird = { git = "https://github.com/serenity-rs/songbird",
             branch = "...",   # exact branch/rev from v1
             features = [...] }
```

Both `adapters-voice` and `adapters-discord` then use:
```toml
serenity = { workspace = true }
songbird = { workspace = true }
```

Run:
```bash
cargo tree | grep -E "^(serenity|songbird)"
```
Expected: one entry for each. Any duplicate means a version override exists
in a crate-level Cargo.toml — find and remove it.

**D2. `dashmap` version consistency**

`adapters-voice` uses `dashmap = "5"`. If `apps/bot` or `adapters-persistence`
also directly reference DashMap, they must use the same major version.

```bash
cargo tree | grep dashmap
```
Expected: one version. A mismatch causes `Arc<DashMap>` to be an incompatible
type across crate boundaries.

**D3. `GatewayIntents::GUILD_MESSAGES` — remove if unused**

The current intents include `GUILD_MESSAGES`. In Serenity, this intent enables
message content events — not needed for slash commands (which use interactions).
Removing it reduces gateway payload size:

```bash
grep -rn "message_create\|MessageEvent\|Message::content" \
    --include="*.rs" crates/adapters-discord/
```

If no results: remove `GatewayIntents::GUILD_MESSAGES` from the intents set.

---

### Findings Report Format

```
## Pass 5.1 Findings Report

### Critical
| ID  | File | Finding | Fixed? |

### High
| ID  | File | Finding | Fixed? |

### Medium
| ID  | File | Deferred reason or action taken |

### Low / Accepted
| ID  | Note |

### Version Alignment
serenity git ref in workspace:  <value>
songbird git ref in workspace:  <value>
adapters-voice uses workspace ref: Yes/No
adapters-discord uses workspace ref: Yes/No

### Error Architecture
VoiceError removed: Yes/No
AppError::Voice variant added: Yes/No
kind_str() updated: Yes/No
Retryable impl updated: Yes/No

### v1 Command Removal
grep output: (empty = pass, any results = action needed)

### Net Diff Summary
Total files changed: N
Lines added: N
Lines removed: N
```

---

### Constraints

- Do not add new slash commands. `/skip`, `/pause`, `/resume` are Pass 6.
- Do not implement position-seeking on channel move — Pass 6.
- Do not persist `now_playing_msg` to DB — Pass 6.
- The `intentional_stop` flag (A2 fix) must be reset to `false` by
  `TrackEndHandler` — never left as `true` permanently.
- All changes must leave `cargo build --workspace` and
  `cargo test --workspace` passing.

---

### REFERENCE

*[Attach full `teamti_v2_master.md` here before sending to agent.]*
