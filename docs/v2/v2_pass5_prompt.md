# TeamTI v2 — Pass 5 Implementation Prompt
## Discord Integration: Music Playback Commands

---

### Context

Passes 1–4.5 are complete. The enrichment pipeline is fully operational.
Tracks in the database with `enrichment_status = 'done'` are searchable via
`TrackSearchPort`. Songbird is declared as a dependency but the voice and
Discord adapter crates contain only Pass 1 stubs.

This pass implements the minimum viable music streaming interface. The goal
is a bot that a user can interact with naturally: call `/play` from a voice
channel, hear audio, and have the bot manage itself from there.

---

### Confirmed Design Decisions

| Decision | Value |
|---|---|
| Auto-leave timeout | `AUTO_LEAVE_SECS` in `.env` (default: 30) |
| Auto-leave trigger | Only when queue is empty; any new queue entry resets the timer |
| `/clear` | Clears pending queue only; current track plays to completion |
| `/leave` | Stop immediately, clear queue, leave channel |
| Bot in different channel than user | Move to user's channel; preserve queue; restart current track from beginning; append new track to end |
| `/play` while queue is non-empty | Appends to end of queue |
| `/search` | Completely unregistered from Discord |
| `/rescan` | Registered with `default_member_permissions = Permissions::ADMINISTRATOR` (invisible to non-admins) |
| Skip / pause / resume | Deferred to Pass 6 |
| Now-playing messages | Posted (not ephemeral) in the text channel of last invocation; auto-edited on track change |
| Channel move — position seek | Not implemented in Pass 5; current track restarts from beginning after channel move (limitation documented) |

---

### Acceptance Criteria

- [ ] `cargo build --workspace` — zero errors, zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] `/play <query>` in a voice channel joins, enqueues, and starts playback
- [ ] `/play <query>` while not in a voice channel replies with an ephemeral error
- [ ] `/play <query>` while bot is in a different channel moves the bot and resumes
- [ ] After a track ends, the next queued track starts automatically
- [ ] When the last track ends, the auto-leave timer starts; bot leaves after `AUTO_LEAVE_SECS`
- [ ] Queuing a track during the auto-leave window cancels the timer and resumes playback
- [ ] `/clear` removes all pending tracks; current track continues unaffected
- [ ] `/leave` stops playback, clears queue, and disconnects immediately
- [ ] Now-playing message is posted on first play and edited on each track change
- [ ] Now-playing message shows "Queue ended" when queue becomes empty
- [ ] `/rescan` is invisible to regular users; server admins can invoke it
- [ ] `/ping` and all non-music v1 commands are removed from the codebase
- [ ] All v1 command registrations are removed from the global command set

---

### Scope

| Crate | Action |
|---|---|
| `crates/adapters-voice/` | **Implement** — GuildMusicState, VoiceManager, TrackEndHandler, audio source |
| `crates/adapters-discord/` | **Implement** — command handlers, autocomplete, embeds, event handler |
| `apps/bot/` | **Extend** — register commands, wire voice + discord adapters |

**Remove from codebase:**
- All v1 commands not related to music (e.g. `/ping`, any test commands)
- Any v1 command registrations in `apps/bot/main.rs` or equivalent

---

### Commands Defined in This Pass

| Command | Options | Access |
|---|---|---|
| `/play` | `query: String` (required, autocomplete) | All |
| `/clear` | — | All |
| `/leave` | — | All |
| `/rescan` | — | Administrator only |

---

### Step 1 — `crates/adapters-voice/`: Guild Music State

This crate owns all per-guild playback state and the Songbird interaction layer.

#### `Cargo.toml`

```toml
[package]
name = "adapters-voice"
version = "0.1.0"
edition = "2021"

[dependencies]
application          = { path = "../application" }
adapters-persistence = { path = "../adapters-persistence" }
domain               = { path = "../domain" }
shared-config        = { path = "../shared-config" }

songbird     = { version = "0.4", features = ["builtin-queue"] }
serenity     = { version = "0.12", default-features = false,
                 features = ["client", "gateway", "rustls_backend", "model"] }
tokio        = { workspace = true, features = ["sync", "time"] }
tokio-util   = { workspace = true, features = ["rt"] }
tracing      = { workspace = true }
uuid         = { workspace = true }
dashmap      = "5"
async-trait  = { workspace = true }
thiserror    = { workspace = true }
```

#### `src/state.rs`

```rust
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serenity::model::id::{ChannelId, GuildId, MessageId};
use songbird::tracks::TrackHandle;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use domain::Track;

#[derive(Debug, Clone)]
pub struct QueuedTrack {
    pub track_id:     Uuid,
    pub title:        String,
    pub artist:       String,
    pub album:        Option<String>,
    pub duration_ms:  Option<i32>,
    pub blob_location: String,
}

impl From<&domain::Track> for QueuedTrack {
    fn from(t: &domain::Track) -> Self {
        QueuedTrack {
            track_id:      t.id,
            title:         t.title.clone(),
            artist:        t.artist_display.clone().unwrap_or_default(),
            album:         None,  // resolved at display time
            duration_ms:   t.duration_ms,
            blob_location: t.blob_location.clone(),
        }
    }
}

pub struct CurrentTrack {
    pub track:    QueuedTrack,
    pub handle:   TrackHandle,
}

pub struct GuildMusicState {
    // Playback
    pub current_track:    Option<CurrentTrack>,
    pub queue:            VecDeque<QueuedTrack>,

    // Voice
    pub voice_channel_id: Option<ChannelId>,

    // Text — channel where now-playing messages are sent
    pub text_channel_id:  Option<ChannelId>,
    pub now_playing_msg:  Option<MessageId>,

    // Auto-leave: cancelled when a new track is queued
    pub auto_leave_token: Option<CancellationToken>,
}

impl GuildMusicState {
    pub fn new() -> Self {
        Self {
            current_track:    None,
            queue:            VecDeque::new(),
            voice_channel_id: None,
            text_channel_id:  None,
            now_playing_msg:  None,
            auto_leave_token: None,
        }
    }

    /// Cancel any running auto-leave timer. Call when a track is queued
    /// or when playback starts.
    pub fn cancel_auto_leave(&mut self) {
        if let Some(token) = self.auto_leave_token.take() {
            token.cancel();
        }
    }

    /// Returns true if there is no current track and the queue is empty.
    pub fn is_idle(&self) -> bool {
        self.current_track.is_none() && self.queue.is_empty()
    }
}
```

#### `src/state_map.rs`

```rust
use std::sync::Arc;
use dashmap::DashMap;
use serenity::model::id::GuildId;
use serenity::prelude::TypeMapKey;
use tokio::sync::Mutex;

use crate::state::GuildMusicState;

/// Per-guild music state. Stored in Serenity's TypeMap.
pub type GuildStateMap = DashMap<GuildId, Arc<Mutex<GuildMusicState>>>;

pub struct GuildStateMapKey;
impl TypeMapKey for GuildStateMapKey {
    type Value = Arc<GuildStateMap>;
}
```

#### `src/player.rs`

This module contains all Songbird interactions: joining channels, playing
audio, and stopping.

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serenity::model::id::{ChannelId, GuildId};
use serenity::prelude::Context;
use songbird::input::File as SongbirdFile;
use songbird::tracks::TrackHandle;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::error::VoiceError;
use crate::state::{CurrentTrack, GuildMusicState, QueuedTrack};
use crate::state_map::GuildStateMapKey;

/// Join the specified voice channel. If already in a different channel,
/// leave first. Returns error if unable to connect.
pub async fn join_channel(
    ctx: &Context,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Result<(), VoiceError> {
    let manager = songbird::get(ctx)
        .await
        .ok_or(VoiceError::SongbirdNotInitialized)?;

    // join() automatically leaves the previous channel if in one
    let (_, join_result) = manager.join(guild_id, channel_id).await;
    join_result.map_err(VoiceError::Join)?;

    info!(
        guild_id  = %guild_id,
        channel_id = %channel_id,
        operation  = "voice.join",
        "joined voice channel"
    );
    Ok(())
}

/// Leave the voice channel for the guild.
pub async fn leave_channel(
    ctx: &Context,
    guild_id: GuildId,
) -> Result<(), VoiceError> {
    let manager = songbird::get(ctx)
        .await
        .ok_or(VoiceError::SongbirdNotInitialized)?;

    manager.leave(guild_id).await.map_err(VoiceError::Leave)?;

    info!(
        guild_id  = %guild_id,
        operation = "voice.leave",
        "left voice channel"
    );
    Ok(())
}

/// Start playing the given track. Returns the TrackHandle.
/// The absolute path to the audio file is constructed from media_root + blob_location.
pub async fn play_track(
    ctx: &Context,
    guild_id: GuildId,
    track: &QueuedTrack,
    media_root: &PathBuf,
) -> Result<TrackHandle, VoiceError> {
    let manager = songbird::get(ctx)
        .await
        .ok_or(VoiceError::SongbirdNotInitialized)?;

    let handler_lock = manager
        .get(guild_id)
        .ok_or(VoiceError::NotInChannel)?;

    let abs_path = media_root.join(&track.blob_location);

    if !abs_path.exists() {
        return Err(VoiceError::FileNotFound(abs_path));
    }

    let source = SongbirdFile::new(abs_path);

    let mut handler = handler_lock.lock().await;

    // Stop any existing track before playing the new one.
    // (This handles the channel-move restart case.)
    handler.stop();

    let handle = handler.play(source.into());

    info!(
        guild_id   = %guild_id,
        track_id   = %track.track_id,
        path       = %track.blob_location,
        operation  = "voice.play",
        "started track playback"
    );

    Ok(handle)
}

/// Stop the current track without leaving the channel.
pub async fn stop_current(
    ctx: &Context,
    guild_id: GuildId,
) -> Result<(), VoiceError> {
    let manager = songbird::get(ctx)
        .await
        .ok_or(VoiceError::SongbirdNotInitialized)?;

    if let Some(handler_lock) = manager.get(guild_id) {
        handler_lock.lock().await.stop();
    }
    Ok(())
}
```

#### `src/error.rs`

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("Songbird is not initialized — check bot startup")]
    SongbirdNotInitialized,

    #[error("failed to join voice channel: {0}")]
    Join(#[from] songbird::error::JoinError),

    #[error("failed to leave voice channel: {0}")]
    Leave(songbird::error::JoinError),

    #[error("bot is not in a voice channel")]
    NotInChannel,

    #[error("audio file not found at {0:?}")]
    FileNotFound(PathBuf),
}
```

#### `src/track_end_handler.rs`

Songbird fires `TrackEvent::End` when a track finishes. This handler
dequeues the next track, starts it, and updates the now-playing message.
It also starts the auto-leave timer when the queue is empty.

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serenity::model::id::{ChannelId, GuildId};
use serenity::prelude::Context;
use songbird::{Event, EventContext, EventHandler as SongbirdEventHandler};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::state::GuildMusicState;
use crate::state_map::GuildStateMapKey;
use crate::player::{play_track, leave_channel};

pub struct TrackEndHandler {
    pub guild_id:        GuildId,
    pub ctx:             Context,
    pub media_root:      PathBuf,
    pub auto_leave_secs: u64,
}

#[async_trait]
impl SongbirdEventHandler for TrackEndHandler {
    async fn act(&self, _event_ctx: &EventContext<'_>) -> Option<Event> {
        let state_map = {
            let data = self.ctx.data.read().await;
            data.get::<GuildStateMapKey>()
                .expect("GuildStateMapKey not in TypeMap")
                .clone()
        };

        let state_lock = match state_map.get(&self.guild_id) {
            Some(s) => s.clone(),
            None    => return None,
        };

        let mut state = state_lock.lock().await;

        // Clear the current track reference — it has ended
        state.current_track = None;

        // Dequeue next track
        if let Some(next_track) = state.queue.pop_front() {
            drop(state); // release lock before the async play call

            match play_track(&self.ctx, self.guild_id, &next_track, &self.media_root).await {
                Ok(handle) => {
                    let mut state = state_lock.lock().await;

                    // Register this handler on the new track
                    handle.add_event(
                        Event::Track(songbird::TrackEvent::End),
                        TrackEndHandler {
                            guild_id:        self.guild_id,
                            ctx:             self.ctx.clone(),
                            media_root:      self.media_root.clone(),
                            auto_leave_secs: self.auto_leave_secs,
                        },
                    ).ok();

                    state.current_track = Some(crate::state::CurrentTrack {
                        track:  next_track.clone(),
                        handle,
                    });

                    // Post/update now-playing message
                    if let Some(channel_id) = state.text_channel_id {
                        let msg_id = state.now_playing_msg;
                        drop(state);
                        post_now_playing(
                            &self.ctx, channel_id, self.guild_id,
                            &state_lock, Some(&next_track), msg_id,
                        ).await;
                    }
                }
                Err(e) => {
                    warn!(
                        guild_id  = %self.guild_id,
                        error     = %e,
                        operation = "track_end.play_next",
                        "failed to start next track"
                    );
                }
            }
        } else {
            // Queue is empty — start auto-leave timer
            let token = CancellationToken::new();
            state.cancel_auto_leave();
            state.auto_leave_token = Some(token.clone());

            let ctx        = self.ctx.clone();
            let guild_id   = self.guild_id;
            let secs       = self.auto_leave_secs;
            let state_lock = state_lock.clone();

            // Update now-playing to show queue ended
            if let Some(channel_id) = state.text_channel_id {
                let msg_id = state.now_playing_msg;
                drop(state);
                post_now_playing(
                    &ctx, channel_id, guild_id,
                    &state_lock, None, msg_id,
                ).await;
            } else {
                drop(state);
            }

            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        // A new track was queued — do nothing
                    }
                    _ = tokio::time::sleep(Duration::from_secs(secs)) => {
                        let mut state = state_lock.lock().await;
                        state.voice_channel_id = None;
                        state.now_playing_msg  = None;
                        drop(state);
                        let _ = leave_channel(&ctx, guild_id).await;
                        info!(
                            guild_id  = %guild_id,
                            operation = "voice.auto_leave",
                            "auto-left voice channel after idle timeout"
                        );
                    }
                }
            });
        }

        None
    }
}

/// Post or edit the now-playing message in the text channel.
/// If `track` is None, posts a "Queue ended" message.
pub async fn post_now_playing(
    ctx:        &Context,
    channel_id: ChannelId,
    guild_id:   GuildId,
    state_lock: &Arc<Mutex<GuildMusicState>>,
    track:      Option<&crate::state::QueuedTrack>,
    existing_msg_id: Option<serenity::model::id::MessageId>,
) {
    use serenity::builder::{CreateEmbed, CreateMessage, EditMessage};

    let embed = build_now_playing_embed(track, state_lock).await;

    match existing_msg_id {
        Some(msg_id) => {
            // Edit existing message
            let edit = EditMessage::new().embed(embed);
            if let Err(e) = channel_id.edit_message(ctx, msg_id, edit).await {
                warn!(
                    guild_id   = %guild_id,
                    channel_id = %channel_id,
                    error      = %e,
                    operation  = "now_playing.edit",
                    "failed to edit now-playing message"
                );
            }
        }
        None => {
            // Post new message
            let msg = CreateMessage::new().embed(embed);
            match channel_id.send_message(ctx, msg).await {
                Ok(sent) => {
                    let mut state = state_lock.lock().await;
                    state.now_playing_msg = Some(sent.id);
                }
                Err(e) => {
                    warn!(
                        guild_id  = %guild_id,
                        error     = %e,
                        operation = "now_playing.post",
                        "failed to post now-playing message"
                    );
                }
            }
        }
    }
}

async fn build_now_playing_embed(
    track:      Option<&crate::state::QueuedTrack>,
    state_lock: &Arc<Mutex<GuildMusicState>>,
) -> serenity::builder::CreateEmbed {
    use serenity::builder::CreateEmbed;

    let state = state_lock.lock().await;
    let queue_len = state.queue.len();
    drop(state);

    match track {
        Some(t) => {
            let duration_str = t.duration_ms
                .map(format_duration)
                .unwrap_or_else(|| "Unknown".to_string());

            let description = format!("**{}**\n{}", t.title, t.artist);

            let mut embed = CreateEmbed::new()
                .title("▶  Now Playing")
                .description(description)
                .color(0x1DB954); // Spotify green — music context

            if let Some(ref album) = t.album {
                embed = embed.field("Album", album, true);
            }
            embed = embed.field("Duration", duration_str, true);

            if queue_len > 0 {
                embed = embed.field(
                    "Up Next",
                    format!("{} track{} in queue", queue_len,
                            if queue_len == 1 { "" } else { "s" }),
                    false,
                );
            }

            embed
        }
        None => {
            CreateEmbed::new()
                .title("Queue Ended")
                .description("No more tracks in queue.")
                .color(0x747F8D) // grey
                .footer(serenity::builder::CreateEmbedFooter::new(
                    "Bot will leave shortly unless a track is queued."
                ))
        }
    }
}

fn format_duration(ms: i32) -> String {
    let total_secs = ms / 1000;
    let minutes    = total_secs / 60;
    let seconds    = total_secs % 60;
    format!("{minutes}:{seconds:02}")
}
```

#### `src/lib.rs`

```rust
pub mod error;
pub mod player;
pub mod state;
pub mod state_map;
pub mod track_end_handler;
```

---

### Step 2 — `crates/adapters-discord/`: Command Handlers

#### `Cargo.toml` additions

```toml
[dependencies]
adapters-voice       = { path = "../adapters-voice" }
adapters-persistence = { path = "../adapters-persistence" }
application          = { path = "../application" }
shared-config        = { path = "../shared-config" }

songbird  = { version = "0.4" }
serenity  = { version = "0.12", default-features = false,
              features = ["client", "gateway", "rustls_backend", "model"] }
tokio     = { workspace = true }
tracing   = { workspace = true }
uuid      = { workspace = true }
```

#### Command module structure

```
adapters-discord/src/
  lib.rs
  commands/
    mod.rs
    play.rs
    clear.rs
    leave.rs
    rescan.rs
  embed.rs          ← (re-export of track_end_handler embed helpers)
  handler.rs        ← Serenity EventHandler impl
```

---

### Step 3 — `/play` Command

The `/play` command has two interaction types: autocomplete and submit.

```rust
// commands/play.rs

use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::model::application::{
    CommandInteraction, CommandOptionType, ResolvedOption, ResolvedValue,
};
use serenity::model::id::GuildId;
use serenity::prelude::Context;

use adapters_voice::{
    player::{join_channel, play_track},
    state::QueuedTrack,
    state_map::GuildStateMapKey,
    track_end_handler::{TrackEndHandler, post_now_playing},
};
use adapters_persistence::TrackRepositoryImpl;
use application::ports::TrackSearchPort;

/// Register the /play command.
pub fn register() -> CreateCommand {
    CreateCommand::new("play")
        .description("Play a track from the library")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "query",
                "Track title or artist",
            )
            .required(true)
            .set_autocomplete(true),
        )
}

/// Handle autocomplete for the `query` option.
pub async fn autocomplete(
    ctx: &Context,
    interaction: &CommandInteraction,
    search_port: &TrackRepositoryImpl,
) {
    use serenity::builder::{AutocompleteChoice, CreateAutocompleteResponse, CreateInteractionResponse};

    let partial = interaction
        .data
        .options
        .iter()
        .find(|o| o.name == "query")
        .and_then(|o| {
            if let ResolvedValue::Autocomplete { value, .. } = &o.value {
                Some(*value)
            } else {
                None
            }
        })
        .unwrap_or("");

    // Use the autocomplete method: prefix match on title
    let suggestions = search_port
        .autocomplete(partial, 25)
        .await
        .unwrap_or_default();

    let choices: Vec<AutocompleteChoice> = suggestions
        .iter()
        .map(|t| {
            let label = match &t.artist_display {
                Some(a) if !a.is_empty() => format!("{} — {}", t.title, a),
                _ => t.title.clone(),
            };
            // value is the track UUID — unambiguous even for duplicate titles
            AutocompleteChoice::new(label, t.id.to_string())
        })
        .collect();

    let response = CreateInteractionResponse::Autocomplete(
        CreateAutocompleteResponse::new().set_choices(choices),
    );
    let _ = interaction.create_response(ctx, response).await;
}

/// Handle the actual /play submission.
pub async fn run(
    ctx:         &Context,
    interaction: &CommandInteraction,
    track_repo:  &TrackRepositoryImpl,
    media_root:  &std::path::PathBuf,
    auto_leave_secs: u64,
) {
    // Acknowledge immediately (defer) — DB + voice ops may take a moment
    let _ = interaction
        .defer_ephemeral(ctx)
        .await;

    // --- 1. Resolve track ---
    let query_value = interaction
        .data
        .options
        .iter()
        .find(|o| o.name == "query")
        .and_then(|o| {
            if let ResolvedValue::String(s) = &o.resolved {
                Some(*s)
            } else {
                None
            }
        });

    let query = match query_value {
        Some(q) => q,
        None => {
            let _ = interaction
                .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                    .content("No query provided."))
                .await;
            return;
        }
    };

    // Try to parse as UUID first (autocomplete selection)
    let track = if let Ok(id) = uuid::Uuid::parse_str(query) {
        track_repo.find_by_id(id).await.ok().flatten()
    } else {
        // Free-text: search and take first result
        track_repo.search(query, 1)
            .await
            .ok()
            .and_then(|mut v| v.pop())
            .and_then(|summary| {
                // summary is TrackSummary; fetch full Track if needed
                // For play purposes, TrackSummary has enough fields
                Some(domain::Track {
                    id:             summary.id,
                    title:          summary.title,
                    artist_display: summary.artist_display,
                    album_id:       summary.album_id,
                    blob_location:  summary.blob_location,
                    duration_ms:    summary.duration_ms,
                    // remaining fields: defaults acceptable for playback
                    ..Default::default()
                })
            })
    };

    let track = match track {
        Some(t) => t,
        None => {
            let _ = interaction
                .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                    .content(format!("No track found matching **{query}**.")))
                .await;
            return;
        }
    };

    // --- 2. Check user is in a voice channel ---
    let guild_id = match interaction.guild_id {
        Some(g) => g,
        None => {
            let _ = interaction
                .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                    .content("This command can only be used in a server."))
                .await;
            return;
        }
    };

    let user_voice_channel = {
        let guild = match guild_id.to_guild_cached(ctx) {
            Some(g) => g,
            None => {
                let _ = interaction
                    .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                        .content("Unable to read guild state."))
                    .await;
                return;
            }
        };
        guild
            .voice_states
            .get(&interaction.user.id)
            .and_then(|vs| vs.channel_id)
    };

    let user_channel = match user_voice_channel {
        Some(c) => c,
        None => {
            let _ = interaction
                .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                    .content("You must be in a voice channel to use `/play`."))
                .await;
            return;
        }
    };

    // --- 3. Voice channel management ---
    let state_map = {
        let data = ctx.data.read().await;
        data.get::<GuildStateMapKey>()
            .expect("GuildStateMapKey not found")
            .clone()
    };

    let state_lock = state_map
        .entry(guild_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(
            adapters_voice::state::GuildMusicState::new()
        )))
        .clone();

    // Determine if we need to move/join channels
    let current_voice = {
        let state = state_lock.lock().await;
        state.voice_channel_id
    };

    let needs_join = current_voice != Some(user_channel);

    if needs_join {
        // If in a different channel: the current track will restart when we
        // re-play it. This is a known limitation — seeking is not supported.
        if current_voice.is_some() {
            let mut state = state_lock.lock().await;
            // Re-queue current track at front if there was one
            if let Some(current) = state.current_track.take() {
                current.handle.stop().ok();
                state.queue.push_front(current.track);
            }
        }

        if let Err(e) = join_channel(ctx, guild_id, user_channel).await {
            let _ = interaction
                .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                    .content(format!("Failed to join your voice channel: {e}")))
                .await;
            return;
        }

        let mut state = state_lock.lock().await;
        state.voice_channel_id = Some(user_channel);
    }

    // --- 4. Enqueue the track ---
    let queued = QueuedTrack::from(&track);
    let queued_title = queued.title.clone();
    let queued_artist = queued.artist.clone();

    {
        let mut state = state_lock.lock().await;

        // Cancel auto-leave timer if running
        state.cancel_auto_leave();

        // Update text channel to current invocation channel
        state.text_channel_id = Some(interaction.channel_id);

        let is_idle = state.is_idle();
        state.queue.push_back(queued.clone());

        if !is_idle {
            // Queue updated; existing track is playing. Nothing else to do here.
            drop(state);
            let _ = interaction
                .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                    .content(format!("Added **{queued_title}** to queue.")))
                .await;
            return;
        }
    }

    // --- 5. Nothing playing — dequeue and start ---
    let to_play = {
        let mut state = state_lock.lock().await;
        state.queue.pop_front()
    };

    if let Some(to_play) = to_play {
        match play_track(ctx, guild_id, &to_play, media_root).await {
            Ok(handle) => {
                // Register TrackEndHandler on this track
                handle.add_event(
                    songbird::Event::Track(songbird::TrackEvent::End),
                    TrackEndHandler {
                        guild_id,
                        ctx:             ctx.clone(),
                        media_root:      media_root.clone(),
                        auto_leave_secs,
                    },
                ).ok();

                {
                    let mut state = state_lock.lock().await;
                    state.current_track = Some(adapters_voice::state::CurrentTrack {
                        track:  to_play.clone(),
                        handle,
                    });
                }

                // Post now-playing message
                let existing_msg = state_lock.lock().await.now_playing_msg;
                post_now_playing(
                    ctx,
                    interaction.channel_id,
                    guild_id,
                    &state_lock,
                    Some(&to_play),
                    existing_msg,
                ).await;

                let _ = interaction
                    .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                        .content(format!("Now playing **{queued_title}** by {queued_artist}.")))
                    .await;
            }
            Err(e) => {
                let _ = interaction
                    .edit_response(ctx, serenity::builder::EditInteractionResponse::new()
                        .content(format!("Failed to start playback: {e}")))
                    .await;
            }
        }
    }
}
```

---

### Step 4 — `/clear` Command

```rust
// commands/clear.rs

pub fn register() -> CreateCommand {
    CreateCommand::new("clear")
        .description("Clear the pending queue. Current track continues playing.")
}

pub async fn run(ctx: &Context, interaction: &CommandInteraction) {
    let _ = interaction.defer_ephemeral(ctx).await;

    let guild_id = match interaction.guild_id {
        Some(g) => g,
        None => {
            let _ = interaction.edit_response(ctx,
                EditInteractionResponse::new().content("Server only.")).await;
            return;
        }
    };

    let state_map = {
        let data = ctx.data.read().await;
        data.get::<GuildStateMapKey>().cloned()
            .expect("GuildStateMapKey not found")
    };

    let cleared = match state_map.get(&guild_id) {
        None => 0,
        Some(state_lock) => {
            let mut state = state_lock.lock().await;
            let n = state.queue.len();
            state.queue.clear();
            n
        }
    };

    let msg = if cleared == 0 {
        "Queue was already empty.".to_string()
    } else {
        format!("Cleared {cleared} track{} from the queue. Current track will finish.",
                if cleared == 1 { "" } else { "s" })
    };

    let _ = interaction.edit_response(ctx,
        EditInteractionResponse::new().content(msg)).await;
}
```

---

### Step 5 — `/leave` Command

```rust
// commands/leave.rs

pub fn register() -> CreateCommand {
    CreateCommand::new("leave")
        .description("Stop playback, clear queue, and leave the voice channel.")
}

pub async fn run(ctx: &Context, interaction: &CommandInteraction) {
    let _ = interaction.defer_ephemeral(ctx).await;

    let guild_id = match interaction.guild_id {
        Some(g) => g,
        None => { return; }
    };

    let state_map = {
        let data = ctx.data.read().await;
        data.get::<GuildStateMapKey>().cloned()
            .expect("GuildStateMapKey not found")
    };

    if let Some(state_lock) = state_map.get(&guild_id) {
        let mut state = state_lock.lock().await;

        // Stop current track
        if let Some(current) = state.current_track.take() {
            current.handle.stop().ok();
        }

        // Clear pending queue
        state.queue.clear();

        // Cancel auto-leave timer
        state.cancel_auto_leave();

        // Clear voice state
        state.voice_channel_id = None;

        // Update now-playing message to show disconnected
        let channel = state.text_channel_id;
        let msg_id  = state.now_playing_msg.take();
        drop(state);

        if let (Some(ch), Some(mid)) = (channel, msg_id) {
            use serenity::builder::{CreateEmbed, EditMessage};
            let embed = CreateEmbed::new()
                .title("Disconnected")
                .description("Playback stopped and queue cleared.")
                .color(0x747F8D);
            let _ = ch.edit_message(ctx, mid, EditMessage::new().embed(embed)).await;
        }
    }

    if let Err(e) = adapters_voice::player::leave_channel(ctx, guild_id).await {
        let _ = interaction.edit_response(ctx,
            EditInteractionResponse::new()
                .content(format!("Left with error: {e}"))).await;
        return;
    }

    let _ = interaction.edit_response(ctx,
        EditInteractionResponse::new().content("Left the voice channel.")).await;
}
```

---

### Step 6 — `/rescan` Command (Admin Only)

```rust
// commands/rescan.rs
use serenity::model::permissions::Permissions;

pub fn register() -> CreateCommand {
    CreateCommand::new("rescan")
        .description("Trigger a library rescan. Admin only.")
        // Invisible to non-administrators in the command list
        .default_member_permissions(Permissions::ADMINISTRATOR)
}

pub async fn run(ctx: &Context, interaction: &CommandInteraction) {
    let _ = interaction.defer_ephemeral(ctx).await;

    // For Pass 5: reset all 'done' tracks' tags_written_at to NULL
    // and log that a rescan was requested. Full rescan trigger is Pass 6+.
    // This is sufficient for testing that the enrichment pipeline
    // can be triggered on-demand.

    tracing::info!(
        user_id    = %interaction.user.id,
        guild_id   = %interaction.guild_id.unwrap_or_default(),
        operation  = "rescan.requested",
        "admin triggered rescan"
    );

    let _ = interaction.edit_response(ctx,
        EditInteractionResponse::new()
            .content("Rescan requested. New files will be processed on next scan cycle.\n\
                      *(This command will be removed in a future version.)*"))
        .await;
}
```

---

### Step 7 — Serenity Event Handler (`handler.rs`)

```rust
// adapters-discord/src/handler.rs

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serenity::model::application::Interaction;
use serenity::model::gateway::Ready;
use serenity::prelude::{Context, EventHandler};

use adapters_persistence::TrackRepositoryImpl;

pub struct DiscordEventHandler {
    pub track_repo:      Arc<TrackRepositoryImpl>,
    pub media_root:      PathBuf,
    pub auto_leave_secs: u64,
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        use serenity::model::application::Command;
        use crate::commands::{play, clear, leave, rescan};

        tracing::info!(
            username   = %ready.user.name,
            guild_count = ready.guilds.len(),
            operation  = "discord.ready",
            "bot connected to Discord"
        );

        // Register slash commands globally.
        // Global registration propagates within ~1 hour.
        // For testing, replace with guild_id.set_commands() for instant registration.
        let result = Command::set_global_commands(&ctx, vec![
            play::register(),
            clear::register(),
            leave::register(),
            rescan::register(),
            // NOTE: /search and all v1 commands intentionally NOT registered.
        ]).await;

        match result {
            Ok(cmds) => tracing::info!(
                count     = cmds.len(),
                operation = "discord.commands_registered",
                "registered slash commands"
            ),
            Err(e) => tracing::error!(
                error     = %e,
                operation = "discord.commands_register_failed",
                "failed to register slash commands"
            ),
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(cmd) => {
                let name = cmd.data.name.as_str();
                tracing::debug!(
                    command    = name,
                    user_id    = %cmd.user.id,
                    guild_id   = %cmd.guild_id.unwrap_or_default(),
                    operation  = "discord.command",
                );
                match name {
                    "play"   => crate::commands::play::run(
                        &ctx, &cmd, &self.track_repo,
                        &self.media_root, self.auto_leave_secs,
                    ).await,
                    "clear"  => crate::commands::clear::run(&ctx, &cmd).await,
                    "leave"  => crate::commands::leave::run(&ctx, &cmd).await,
                    "rescan" => crate::commands::rescan::run(&ctx, &cmd).await,
                    unknown  => tracing::warn!(
                        command   = unknown,
                        operation = "discord.unknown_command",
                        "received unknown command"
                    ),
                }
            }

            Interaction::Autocomplete(ac) => {
                if ac.data.name == "play" {
                    crate::commands::play::autocomplete(
                        &ctx, &ac, &self.track_repo,
                    ).await;
                }
            }

            _ => {}
        }
    }
}
```

---

### Step 8 — Wire in `apps/bot/main.rs`

```rust
use adapters_discord::handler::DiscordEventHandler;
use adapters_voice::state_map::{GuildStateMap, GuildStateMapKey};
use serenity::prelude::GatewayIntents;
use songbird::SerenityInit;

// Build the Discord client
let intents = GatewayIntents::GUILDS
    | GatewayIntents::GUILD_VOICE_STATES
    | GatewayIntents::GUILD_MESSAGES;

let handler = DiscordEventHandler {
    track_repo:      Arc::clone(&track_repo),
    media_root:      config.media_root.clone(),
    auto_leave_secs: config.auto_leave_secs,
};

let mut client = serenity::Client::builder(&config.discord_token, intents)
    .event_handler(handler)
    .register_songbird()   // Registers Songbird as a voice backend
    .await
    .context("failed to create Discord client")?;

// Insert GuildStateMap into TypeMap
{
    let mut data = client.data.write().await;
    let state_map: Arc<GuildStateMap> = Arc::new(dashmap::DashMap::new());
    data.insert::<GuildStateMapKey>(state_map);
    // Also insert SMB semaphore (already done in Pass 2 — verify it is here)
    data.insert::<adapters_media_store::scanner::SmbSemaphoreKey>(
        Arc::clone(&smb_semaphore)
    );
}

// Start the Discord client (blocks until shutdown signal)
let shard_manager = client.shard_manager.clone();
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.ok();
    token.cancel();
    shard_manager.shutdown_all().await;
});

client.start().await.context("Discord client crashed")?;
```

Add to `shared-config/src/lib.rs`:
```rust
/// Seconds to wait after queue empty before leaving voice channel.
/// Default: 30
pub auto_leave_secs: u64,
```

Add to `.env`:
```
AUTO_LEAVE_SECS=30
```

---

### Step 9 — Remove All v1 Commands

Search and delete:

```bash
# Find all v1 command definitions and registrations
grep -rn "ping\|CreateCommand.*ping\|\"ping\"" --include="*.rs" .
```

Remove every file in `adapters-discord/src/commands/` that contains non-music
commands (ping, info, help, test, etc.). Remove their registrations from any
prior `set_global_commands` calls.

After deletion, ensure `cargo build --workspace` still passes.

---

### Step 10 — Integration Smoke Test

Add `tests/discord_smoke.rs` (compile-only test — no live Discord connection):

```rust
// Verify command definitions compile and have correct options
#[test]
fn play_command_has_query_option() {
    let cmd = adapters_discord::commands::play::register();
    assert_eq!(cmd.name, "play");
    assert!(cmd.options.iter().any(|o| o.name == "query"));
}

#[test]
fn rescan_command_has_admin_permission() {
    use serenity::model::permissions::Permissions;
    let cmd = adapters_discord::commands::rescan::register();
    assert_eq!(
        cmd.default_member_permissions,
        Some(Permissions::ADMINISTRATOR)
    );
}

#[test]
fn search_command_is_not_registered() {
    let commands = vec![
        adapters_discord::commands::play::register(),
        adapters_discord::commands::clear::register(),
        adapters_discord::commands::leave::register(),
        adapters_discord::commands::rescan::register(),
    ];
    assert!(commands.iter().all(|c| c.name != "search"));
    assert!(commands.iter().all(|c| c.name != "ping"));
}

#[test]
fn guild_music_state_initializes_idle() {
    let state = adapters_voice::state::GuildMusicState::new();
    assert!(state.is_idle());
    assert!(state.queue.is_empty());
    assert!(state.current_track.is_none());
}
```

---

### Known Limitations (Documented, Not Bugs)

| Limitation | Detail | Resolution |
|---|---|---|
| Channel move restarts current track | Songbird stream cannot seek after channel rejoin | Pass 6: store position, create seeking Input source |
| Now-playing message not deleted on crash/restart | Stale messages accumulate across restarts | Pass 6: track message ID in DB, attempt cleanup on startup |
| Queue not persisted across restarts | Fully in-memory | By design for v2; can add Redis persistence later |
| `/rescan` does not immediately trigger scan cycle | Logs request only | Pass 6: expose a rescan channel signal to MediaScanner |

---

### Invariants (Pass 5 Specific)

| Rule | Detail |
|---|---|
| `GatewayIntents::GUILD_VOICE_STATES` is required | Without it, `guild.voice_states` is empty and user channel detection fails |
| `register_songbird()` must be called on the client builder | Absent = `songbird::get(&ctx)` returns None = all voice ops panic |
| `interaction.defer_ephemeral()` before any async work | Prevents "interaction expired" errors for operations > 3 seconds |
| Auto-leave token is cancelled before being replaced | Always call `state.cancel_auto_leave()` before assigning a new token |
| `TrackEndHandler` must be registered per-handle, not per-bot | Handlers are consumed per track; re-register on every new `play_track` call |
| `/search` must not appear in `set_global_commands` | Unregistered = invisible to all users regardless of permissions |

---

### REFERENCE

docs/v2/v2_master.md