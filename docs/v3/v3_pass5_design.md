# TeamTI v3 — Pass 5 Design Spec
## Queue, Skip, Pause/Resume, Now Playing & Discord UX Layer

> All decisions are locked. Attach alongside all Pass 4 output files,
> `state.rs`, `player.rs`, `lifecycle_worker.rs`, and the existing
> slash command implementations before sending to the agent.

---

## Decision Log

| Topic | Decision |
|-------|----------|
| `/stop` | Removed entirely. `/pause` freezes, `/resume` continues, `/leave` ends session |
| `/skip` | `/skip` alone = next track; `/skip <n or name>` = skip to that position (autocomplete shows `N. Title — Artist`) |
| Queue modification | All: remove, move, shuffle, clear, save as playlist |
| Queue modification permissions | Anyone in the VC |
| `/queue clear` | Alias for `/clear`. Direct, no confirmation prompt |
| `/queue save` | Creates a new playlist from current queue via existing `PlaylistPort` |
| NP auto-update | Edits the message every 5 seconds while track plays |
| NP post trigger | Auto-posted on every `TrackStarted` lifecycle event |
| NP target channel | Channel where the most recent `/play` was invoked — stored in `GuildMusicState` |
| NP old message on track change | Left stale — not deleted or edited |
| Radio queue label | 🎲 prefix on radio-added tracks |
| Playlist queue label | None — treated as individual tracks once queued |
| Pause duration tracking | Track it (Option A): `paused_at + total_paused_ms` subtracted from play duration |
| Skip negative signal | Skips already handled by completion threshold; verify it propagates to recommendations |
| Queue embed buttons | ⏸/▶ toggle, ⏭ skip, 🔀 shuffle, 🗑️ clear — visible on queue embed |
| Queue display per track | Position, ▶ marker, title + artist (must-have); duration, added_by (nice-to-have) |
| NP embed duplicate name disambiguation | Show `N. Title — Artist` in autocomplete |

---

## State Changes — `GuildMusicState`

Add to `crates/adapters-voice/src/state.rs`:

```rust
/// Timestamp when the current track started or resumed (not counting paused time).
/// Reset to Some(Instant::now()) on TrackStarted and on Resume.
pub track_play_started_at: Option<Instant>,

/// Timestamp when the current track was paused.
/// None when not paused.
pub paused_at: Option<Instant>,

/// Accumulated paused duration for the current track.
/// Reset to 0 on TrackStarted.
pub total_paused_ms: u32,

/// Channel ID where the most recent /play was invoked.
/// Used for auto-posting the Now Playing embed on track change.
pub last_play_channel_id: Option<serenity::model::id::ChannelId>,

/// Message ID of the currently active Now Playing embed.
/// None if no NP embed has been posted yet.
pub nowplaying_message_id: Option<serenity::model::id::MessageId>,

/// Cancellation token for the NP auto-update background task.
/// Cancelled when a new track starts or the bot leaves.
pub np_update_cancel: Option<tokio_util::sync::CancellationToken>,
```

### Pause duration tracking

```rust
// On /pause:
state.paused_at = Some(Instant::now());
// (do NOT reset track_play_started_at)

// On /resume:
if let Some(pa) = state.paused_at.take() {
    let paused_ms = pa.elapsed().as_millis() as u32;
    state.total_paused_ms += paused_ms;
}
// track_play_started_at remains unchanged

// On TrackEnded (in track_event_handler.rs):
let elapsed_ms = state.track_play_started_at
    .map(|s| s.elapsed().as_millis() as u32)
    .unwrap_or(0);
let actual_play_ms = elapsed_ms.saturating_sub(state.total_paused_ms);
// Pass actual_play_ms as play_duration_ms to close_listen_event

// On TrackStarted:
state.track_play_started_at = Some(Instant::now());
state.paused_at = None;
state.total_paused_ms = 0;
```

---

## Queue Metadata

The in-memory queue in `GuildMusicState` must store rich metadata
alongside Songbird's `TrackHandle` list. Extend the existing queue
entry type (or create it if not yet formalised):

```rust
/// Metadata for a single entry in the guild's playback queue.
#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub track_id:    uuid::Uuid,
    pub title:       String,
    pub artist:      String,
    pub duration_ms: u32,         // 0 if unknown
    pub added_by:    String,      // Discord user ID (snowflake as String)
    pub source:      QueueSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueSource {
    /// Added by a user via /play or /playlist play.
    Manual,
    /// Added silently by the radio refill engine.
    Radio,
}
```

`GuildMusicState.queue_meta: Vec<QueueEntry>` — always kept in sync
with Songbird's internal `TrackQueue`. Index 0 = currently playing.

Whenever `TrackQueue::modify_queue` is called, `queue_meta` must be
modified in the same operation (same lock scope) to stay consistent.

---

## New and Modified Commands

### `/skip [target]`

```
/skip              — skip current track, advance to next
/skip 3            — skip forward 3 positions (discard 2 tracks between)
/skip Hotel Ca...  — autocomplete by queue position + title, skip to that entry
```

Autocomplete format: `"3. Hotel California — Eagles"`

When skipping to position N, all tracks before N are **dequeued and
discarded** (their listen events closed as incomplete — completion
threshold handles signal quality). The track at position N becomes
current.

Response: ephemeral `⏭ Skipped to **Hotel California**.`

If queue has only 1 track and `/skip` is called: stop playback,
post ephemeral `Queue is empty after skip.`

### `/pause`

Calls `TrackQueue::pause()`. Records `paused_at` in state.

Response: ephemeral `⏸ Paused.`

If already paused: ephemeral `Already paused. Use /resume to continue.`
If nothing playing: ephemeral `Nothing is currently playing.`

### `/resume`

Calls `TrackQueue::resume()`. Accumulates paused time, clears `paused_at`.

Response: ephemeral `▶ Resumed.`

If not paused: ephemeral `Playback isn't paused.`
If nothing in queue: ephemeral `Queue is empty.`

### `/leave` (existing — verify it handles new state)

On leave, must:
1. Cancel `np_update_cancel` token
2. Clear `queue_meta`
3. Close all open listen events with elapsed time
4. Reset `total_paused_ms`, `paused_at`, `track_play_started_at`
5. Disconnect via Songbird

### `/nowplaying`

Posts a new NP embed to the current channel. Also resets the
auto-update anchor to this new message (cancels old task, starts new
one targeting this message).

If nothing is playing: ephemeral `Nothing is currently playing.`

### `/queue` (new)

Paginated queue view with inline action buttons. See §NP & Queue Embeds.

### `/queue remove <position>`

Autocomplete: `"3. Hotel California — Eagles"` (same format as /skip).
Removes the entry at that position from both `queue_meta` and Songbird queue.
Cannot remove position 0 (currently playing) — use `/skip` instead.

Response: ephemeral `🗑️ Removed **Hotel California** from the queue.`

### `/queue move <from> <to>`

Autocomplete on `<from>`: same position format.
`<to>`: integer (1-based, excluding position 0).

Response: ephemeral `✓ Moved **Hotel California** to position 3.`

### `/queue shuffle`

Shuffles positions 1..end of `queue_meta` and reorders Songbird's
queue via `modify_queue`. Position 0 (currently playing) is untouched.

Response: ephemeral `🔀 Queue shuffled.`

### `/queue clear` / `/clear`

Removes all tracks from positions 1..end. Position 0 (currently
playing) is NOT removed — use `/skip` to end it.

Response: ephemeral `🗑️ Queue cleared.`

### `/queue save [name]`

Saves current queue (all entries including position 0) as a new
playlist via `PlaylistPort::create_playlist` + `add_track`.

If `name` is omitted, defaults to `"Queue {YYYY-MM-DD}"`.

On `AlreadyExists`: ephemeral `A playlist named "{name}" already exists.
Try /queue save <different-name>.`

On success: ephemeral `✅ Saved **12 tracks** as playlist **"{name}"**.`

---

## Now Playing Embed

### Visual layout

```
╭──────────────────────────────────────────────────╮
│  ▶  NOW PLAYING                     [album art]  │
│                                                   │
│  Never Gonna Give You Up                          │
│  Rick Astley • 1987                               │
│                                                   │
│  ━━━━━━━━━━━●──────────────── 2:34 / 4:12        │
│                                                   │
│  Next: Bohemian Rhapsody — Queen                  │
│  Queue: 8 tracks  •  Added by @rickroll_enjoyer   │
╰──────────────────────────────────────────────────╯
```

**Embed fields:**
- Title: track title (link to MusicBrainz recording URL if MBID exists)
- Description: `{artist}  •  {year}` (year from tags if available)
- Thumbnail: cover art URL (from `adapters-coverart`, may be None)
- Color: `0x1DB954` (green) when playing, `0xFAA61A` (amber) when paused

**Progress bar generation:**

```rust
/// Generates a Unicode progress bar.
/// filled_char: ━   cursor_char: ●   empty_char: ─
/// bar_width: 20 characters
fn progress_bar(elapsed_ms: u32, total_ms: u32, width: usize) -> String {
    if total_ms == 0 { return "─".repeat(width); }
    let ratio = (elapsed_ms as f64 / total_ms as f64).clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let filled = filled.min(width - 1);  // always leave room for cursor
    format!(
        "{}{}{}",
        "━".repeat(filled),
        "●",
        "─".repeat(width - filled - 1)
    )
}

// Display: "{bar}  {elapsed}  /  {total}"
// Format time as M:SS (e.g. "2:34")
fn format_duration_ms(ms: u32) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}
```

**Footer fields:** (use embed footer + timestamp)
- Footer text: `Next: {title} — {artist}` if queue has ≥2 entries, else `No tracks queued`
- Timestamp: the moment the embed was last refreshed (Discord renders as "Today at 2:34 PM")

**Paused state variant:**
- Embed title prefix: `⏸  PAUSED` instead of `▶  NOW PLAYING`
- Color: amber `0xFAA61A`
- Progress bar shows `⏸` at cursor position instead of `●`

### Auto-update task

```rust
// Spawned on every TrackStarted lifecycle event.
// Cancelled by CancellationToken when:
//   - A new track starts (new task spawned for new message)
//   - Bot leaves the VC (/leave handler cancels token)

async fn np_auto_update_task(
    cancel:     CancellationToken,
    http:       Arc<serenity::http::Http>,
    channel_id: ChannelId,
    message_id: MessageId,
    state:      Arc<Mutex<GuildMusicState>>,
) {
    let interval = Duration::from_secs(5);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                let embed = build_np_embed(&*state.lock().await);
                let _ = http
                    .edit_message(channel_id, message_id, &json!({ "embeds": [embed] }), &[])
                    .await;
                // Ignore edit errors (message may have been deleted by user)
            }
        }
    }
}
```

### NP lifecycle in `lifecycle_worker.rs`

```
On TrackStarted { guild_id, track_id, users_in_channel }:
  1. Build NP embed from current GuildMusicState
  2. Post embed to state.last_play_channel_id
     If last_play_channel_id is None: skip NP post (no channel known yet)
  3. Store message_id in state.nowplaying_message_id
  4. Cancel existing state.np_update_cancel if Some
  5. Create new CancellationToken, store in state.np_update_cancel
  6. Spawn np_auto_update_task with the new token and message_id
```

---

## Queue Embed

### Visual layout (10 tracks per page)

```
╭──────────────────────────────────────────────────╮
│  📋  QUEUE  —  12 tracks  •  ~42 min remaining   │
│                                                   │
│  ▶  1.  Never Gonna Give You Up                   │
│         Rick Astley  •  3:32  •  @rickroll        │
│                                                   │
│      2.  Bohemian Rhapsody                        │
│          Queen  •  5:54  •  @user2                │
│                                                   │
│      3.  🎲 Hotel California                      │
│          Eagles  •  6:30  •  radio                │
│    ...                                            │
│                                                   │
│  [◀ Prev]   Page 1 / 2   [Next ▶]                │
│  [⏸ Pause]  [⏭ Skip]  [🔀 Shuffle]  [🗑️ Clear]  │
╰──────────────────────────────────────────────────╯
```

**Embed construction:**

- Title: `📋  QUEUE  —  {n} tracks  •  ~{total_remaining_min} min remaining`
  - `total_remaining_min` = sum of `duration_ms` for positions 1..end (not position 0)
- Each track rendered as a single field (inline: false):
  - Marker: `▶` for position 0 (currently playing), numeric for others
  - Title: bold track title
  - Subline: `{artist}  •  {duration_str}  •  @{added_by_username}`
    - For radio-added: `{artist}  •  {duration_str}  •  🎲 radio`
- Color: `0x5865F2` (Discord blurple) — neutral, not state-dependent

**Action buttons (second component row):**

```
Row 1: [◀ Prev]  [Page N/M — disabled]  [Next ▶]
Row 2: [⏸/▶ Pause/Resume]  [⏭ Skip]  [🔀 Shuffle]  [🗑️ Clear]
```

Button states:
- `⏸/▶` label reflects current paused/playing state
- `⏭ Skip` — disabled if queue has exactly 1 track (nothing to skip to)
- `🔀 Shuffle` — disabled if ≤1 track in queue after current
- `🗑️ Clear` — disabled if queue is empty (only current track)
- `◀ Prev` — disabled on page 1; `Next ▶` disabled on last page

**Button custom_id format:**

```
"queue_prev:{guild_id}:{page}:{invoking_user_id}"
"queue_next:{guild_id}:{page}:{invoking_user_id}"
"queue_pause:{guild_id}:{invoking_user_id}"
"queue_skip:{guild_id}:{invoking_user_id}"
"queue_shuffle:{guild_id}:{invoking_user_id}"
"queue_clear:{guild_id}:{invoking_user_id}"
```

Unlike playlist pagination (Pass 3), queue action buttons are NOT
user-gated — anyone can press them (Q8). The `invoking_user_id` in
pagination custom_ids is kept for the Prev/Next buttons only (so the
page-flip session is user-owned), but action buttons (pause, skip,
shuffle, clear) accept input from any user.

After any action button press, re-render and edit the queue message
with the updated state.

**Queue embed freshness note:**
The queue embed is a snapshot — it does not auto-update when tracks
are added or the currently playing track advances. If the user wants
the latest state, they run `/queue` again. Only pagination and action
buttons cause re-renders.

---

## Error Messages — Full Catalogue

All error responses are ephemeral.

| Condition | Message |
|-----------|---------|
| Bot not in VC | `The bot isn't in a voice channel. Use /play to start.` |
| User not in VC | `You need to be in a voice channel to use this.` |
| Queue empty | `The queue is empty.` |
| Nothing playing | `Nothing is currently playing.` |
| Already paused | `Already paused. Use /resume to continue.` |
| Not paused | `Playback isn't paused.` |
| Skip on single track | `Queue is empty after skip.` |
| Remove position 0 | `Can't remove the currently playing track. Use /skip instead.` |
| Invalid position | `Position {n} doesn't exist in the queue.` |
| Save name conflict | `A playlist named "{name}" already exists. Try /queue save <other-name>.` |

---

## `GuildMusicState` — `last_play_channel_id` Update

In `commands/play.rs`, at the point the play command is successfully
acknowledged (before or after the track is added to the queue):

```rust
// After confirming the user is in VC and the track was enqueued:
let mut state = guild_state.lock().await;
state.last_play_channel_id = Some(ctx.channel_id());
```

This ensures NP auto-posts target the correct channel even if the
bot was originally started from a different channel.

---

## NP Skip-Signal Verification

Q4 confirmed: skips are handled by the completion threshold.
Before closing the implementation, verify:

1. When `/skip` is called, `TrackEnded` fires via Songbird's event system.
2. `track_event_handler.rs` computes `actual_play_ms` (with pause
   duration subtracted per the new formula above).
3. `lifecycle_worker.rs` calls `close_listen_event(user_id, track_id,
   actual_play_ms, track_duration_ms)`.
4. `completed = actual_play_ms / track_duration_ms >= LISTEN_COMPLETION_THRESHOLD`.
5. A skipped-after-5s track produces `completed = false`.
6. `AffinityUpdate` is NOT dispatched for incomplete listens (see Pass 4.1 M5).

Add a manual verification step: play a track, skip immediately, confirm
the listen event in the DB has `completed = false` and `play_duration_ms`
reflects the actual short duration.

---

## Crates Affected

| Crate | Changes |
|-------|---------|
| `crates/adapters-voice` | `state.rs`: new fields; `track_event_handler.rs`: pause-aware duration |
| `crates/adapters-discord` | New: `nowplaying.rs`, updated `queue.rs`, `skip.rs`, `pause.rs`, `resume.rs`; `lifecycle_worker.rs`: NP auto-post task |
| `crates/adapters-discord` | `pagination.rs`: extend custom_id handling for queue action buttons |
| `apps/bot` | `main.rs`: register new commands; `handler.rs`: route new interactions |

No new crates required.

---

## Dependency Addition

`tokio-util` with the `sync` feature (for `CancellationToken`).
Add to `adapters-discord/Cargo.toml` and `adapters-voice/Cargo.toml`:

```toml
tokio-util = { workspace = true, features = ["sync"] }
```

---

## Verification Plan

```bash
cargo build --workspace 2>&1 | grep -E "^error|^warning"
cargo test --workspace

# Manual checklist:
# 1. /play 3 tracks → /queue shows all 3 with correct positions and ▶ marker
# 2. /pause → embed updates to ⏸, color turns amber
# 3. /resume → embed updates to ▶, color turns green
# 4. /skip → track advances, new NP embed posted, old one left stale
# 5. /skip 2 → jumps to position 2, position 1 discarded
# 6. /skip "Hotel" → autocomplete shows position+title, skips correctly
# 7. Queue ⏭ button → same as /skip, queue embed re-renders
# 8. Queue 🔀 button → order changes, queue embed re-renders
# 9. Queue 🗑️ button → all but current cleared, queue embed re-renders
# 10. /queue save "My Mix" → playlist created, 2nd call same name → AlreadyExists error
# 11. NP auto-updates: play a track, watch the progress bar advance every 5s
# 12. /leave → NP task cancelled, no further edits, bot disconnects
# 13. Pause 10 minutes, resume, skip at 30s → DB shows ~30s play_duration, completed=false
# 14. Play to 85% → DB shows completed=true, AffinityUpdate fires
```
