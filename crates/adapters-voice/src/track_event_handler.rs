use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serenity::all::Http;
use serenity::model::id::{ChannelId, GuildId, MessageId};
use songbird::{Event, EventContext, EventHandler as SongbirdEventHandler};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::lifecycle::{TrackLifecycleEvent, TrackLifecycleTx};
use crate::player::leave_channel;
use crate::state::{GuildMusicState, QueueSource, QueuedTrack};
use crate::state_map::GuildStateMap;

/// Attached to every track's `TrackEvent::End` and `TrackEvent::Error`.
///
/// Songbird's builtin queue handles audio advancement automatically.
/// This handler is responsible only for:
///  1. Popping the finished track from our parallel `meta_queue`.
///  2. Updating the "Now Playing" Discord embed.
///  3. Starting the auto-leave timer when the queue drains.
///  4. Emitting TrackLifecycleEvent for listen event closure + radio refill.
#[derive(Clone)]
pub struct TrackEventHandler {
    pub guild_id: GuildId,
    pub http: Arc<Http>,
    pub cache: Arc<serenity::all::Cache>,
    pub auto_leave_secs: u64,
    pub songbird: Arc<songbird::Songbird>,
    pub state_map: Arc<GuildStateMap>,
    pub lifecycle_tx: TrackLifecycleTx,
}

#[async_trait]
impl SongbirdEventHandler for TrackEventHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        // Log track decode errors
        if let EventContext::Track(ts) = ctx {
            let is_error = matches!(
                ts.first().map(|ts| &ts.0.playing),
                Some(songbird::tracks::PlayMode::Errored(_))
            );
            if is_error {
                warn!(
                    guild_id   = %self.guild_id,
                    error.kind = "voice.track_decode_error",
                    operation  = "track_event_handler.error",
                    "track ended with a decode error"
                );
            }
        }

        let state_lock = match self.state_map.get(&self.guild_id) {
            Some(s) => s.clone(),
            None => return None,
        };

        let mut state = state_lock.lock().await;

        let target_uuid = if let EventContext::Track(ts) = ctx {
            ts.first().map(|t| t.1.uuid())
        } else {
            None
        };

        // Pop the just-finished track from our metadata mirror by matching UUID securely.
        // Songbird already popped its own TrackQueue entry via QueueHandler.
        let finished_track = if let Some(uuid) = target_uuid {
            let pos = state
                .meta_queue
                .iter()
                .position(|t| t.songbird_uuid == Some(uuid));
            match pos {
                Some(p) => state.meta_queue.remove(p)?,
                None => return None, // Track removed already or invalid — don't double-post
            }
        } else {
            return None;
        };

        // ── Emit TrackEnded lifecycle event (pause-aware duration) ──────
        let play_duration_ms = state.actual_play_ms();

        // Push to history unless suppressed by a Prev button command
        if state.suppress_history_push {
            state.suppress_history_push = false;
        } else {
            state.history.push_back(finished_track.clone());
            if state.history.len() > 50 {
                state.history.pop_front();
            }
        }

        let _ = self.lifecycle_tx.send(TrackLifecycleEvent::TrackEnded {
            guild_id: self.guild_id,
            track_id: finished_track.track_id,
            track_duration_ms: finished_track.duration_ms,
            play_duration_ms,
        });

        // Cancel the NP auto-update task for the finished track
        state.cancel_np_update();

        if state.meta_queue.is_empty() {
            // ── Check radio refill before auto-leave ──────────────────
            if state.radio_mode
                && let Some(user_id) = &state.radio_user_id
            {
                let _ = self
                    .lifecycle_tx
                    .send(TrackLifecycleEvent::RadioRefillNeeded {
                        guild_id: self.guild_id,
                        user_id: user_id.clone(),
                        seed_track_id: Some(finished_track.track_id),
                    });
            }

            // Queue drained — start the idle/auto-leave timer
            let token = CancellationToken::new();
            state.cancel_auto_leave();
            state.auto_leave_token = Some(token.clone());
            state.track_started_at = None;
            state.paused_at = None;
            state.total_paused_ms = 0;

            let http = Arc::clone(&self.http);
            let guild_id = self.guild_id;
            let secs = self.auto_leave_secs;
            let state_lock = state_lock.clone();
            let songbird = Arc::clone(&self.songbird);

            if let Some(channel_id) = state.text_channel_id {
                let msg_id = state.now_playing_msg;
                drop(state);
                post_now_playing(&http, channel_id, guild_id, &state_lock, None, msg_id).await;
            } else {
                drop(state);
            }

            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    () = token.cancelled() => {
                        // A new track was enqueued — cancel timer
                    }
                    () = tokio::time::sleep(Duration::from_secs(secs)) => {
                        // C4 audit fix: use shared cleanup to match /leave behavior
                        let mut state = state_lock.lock().await;
                        state.cleanup_on_leave();
                        drop(state);
                        let _ = leave_channel(&songbird, guild_id).await;
                        info!(
                            guild_id  = %guild_id,
                            operation = "voice.auto_leave",
                            "auto-left voice channel after idle timeout"
                        );
                    }
                }
            });
        } else {
            // More tracks remain. Songbird has already started the next one.
            // Update the "Now Playing" embed to the new front of the queue.
            let next_track = state.meta_queue.front().cloned();

            // Reset timing for the new track
            state.reset_track_timing();

            // ── Radio refill check: ≤ RADIO_REFILL_THRESHOLD tracks remaining ──
            if state.radio_mode
                && state.meta_queue.len() <= application::RADIO_REFILL_THRESHOLD
                && let Some(user_id) = &state.radio_user_id
            {
                let _ = self
                    .lifecycle_tx
                    .send(TrackLifecycleEvent::RadioRefillNeeded {
                        guild_id: self.guild_id,
                        user_id: user_id.clone(),
                        seed_track_id: Some(finished_track.track_id),
                    });
            }

            // ── Emit TrackStarted for the new track ───────────────────
            if let Some(ref next) = next_track {
                let mut users_in_channel = Vec::new();
                if let Some(channel_id) = state.voice_channel_id {
                    let cache = self.cache.clone();
                    users_in_channel = cache
                        .guild(self.guild_id)
                        .map(|g| {
                            g.voice_states
                                .iter()
                                .filter(|vs| vs.channel_id == Some(channel_id))
                                .filter(|vs| {
                                    !g.members.get(&vs.user_id).is_some_and(|m| m.user.bot())
                                })
                                .map(|vs| vs.user_id.to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                }

                let _ = self.lifecycle_tx.send(TrackLifecycleEvent::TrackStarted {
                    guild_id: self.guild_id,
                    track_id: next.track_id,
                    track_duration_ms: next.duration_ms,
                    users_in_channel,
                });
            }

            // ── Retire the old NP message ─────────────────────────────
            // Edit the old NP message to strip interactive buttons so
            // it no longer looks "active". The lifecycle worker will post
            // a brand-new NP message for the next track.
            let old_msg_id = state.now_playing_msg.take();
            let text_channel = state.text_channel_id;
            drop(state);

            if let (Some(channel_id), Some(msg_id)) = (text_channel, old_msg_id) {
                // Build a minimal "finished" edit — strip components
                let finished_embed = build_np_embed(Some(&finished_track), &GuildMusicState::new())
                    .title("⏹  PLAYED")
                    .color(0x0074_7F8D); // grey

                let edit = serenity::builder::EditMessage::new()
                    .embed(finished_embed)
                    .components(vec![]);

                let _ = edit
                    .execute(&self.http, channel_id.into(), msg_id, None)
                    .await;
            }
        }

        None
    }
}

// ── NP Auto-Update Task ─────────────────────────────────────────────────

/// Background task that edits the NP message every 5 seconds with
/// an updated progress bar. Cancelled by CancellationToken when a new
/// track starts or the bot leaves.
pub async fn np_auto_update_task(
    cancel: CancellationToken,
    http: Arc<Http>,
    channel_id: ChannelId,
    guild_id: GuildId,
    message_id: MessageId,
    state: Arc<Mutex<GuildMusicState>>,
) {
    let interval = Duration::from_secs(1);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {
                // O1 audit fix: skip the edit when paused — nothing has
                // changed visually, saves Discord API budget.
                let (embed, is_paused, history_empty) = {
                    let s = state.lock().await;
                    if s.is_paused() {
                        continue;
                    }
                    let track = s.meta_queue.front();
                    (build_np_embed(track, &s), s.is_paused(), s.history.is_empty())
                };

                let action_row = build_np_action_buttons(guild_id, is_paused, history_empty);

                let edit = serenity::builder::EditMessage::new()
                    .embed(embed)
                    .components(vec![action_row]);

                let _ = edit
                    .execute(&http, channel_id.into(), message_id, None)
                    .await;
                // Ignore edit errors (message may have been deleted by user)
            }
        }
    }
}

fn build_np_action_buttons(
    guild_id: GuildId,
    is_paused: bool,
    history_empty: bool,
) -> serenity::builder::CreateComponent<'static> {
    use serenity::builder::{CreateActionRow, CreateButton, CreateComponent};
    use serenity::model::application::ButtonStyle;

    let prev = CreateButton::new(format!("np|prev|{guild_id}"))
        .label("⏮ Prev")
        .style(ButtonStyle::Secondary)
        .disabled(history_empty);

    let pause_resume = CreateButton::new(format!("np|pause|{guild_id}"))
        .label(if is_paused { "▶ Resume" } else { "⏸ Pause" })
        .style(if is_paused {
            ButtonStyle::Success
        } else {
            ButtonStyle::Secondary
        });

    let skip = CreateButton::new(format!("np|skip|{guild_id}"))
        .label("⏭ Next")
        .style(ButtonStyle::Secondary);

    CreateComponent::ActionRow(CreateActionRow::Buttons(
        vec![prev, pause_resume, skip].into(),
    ))
}

// ── NP Embed Construction ───────────────────────────────────────────────

/// Build a rich Now Playing embed with progress bar, state colors, and
/// next-up footer.
pub fn build_np_embed<'a>(
    track: Option<&QueuedTrack>,
    state: &GuildMusicState,
) -> serenity::builder::CreateEmbed<'a> {
    use serenity::builder::{CreateEmbed, CreateEmbedFooter};

    match track {
        Some(t) => {
            let is_paused = state.is_paused();

            // Title prefix
            let title = if is_paused {
                "⏸  PAUSED"
            } else if state.radio_mode {
                "📻  NOW PLAYING (Radio)"
            } else if t.source == QueueSource::YouTube {
                "📺  NOW PLAYING (YouTube)"
            } else {
                "▶  NOW PLAYING"
            };

            // Color: green playing, amber paused
            let color = if is_paused { 0x00FA_A61A } else { 0x001D_B954 };

            // Description: track title + artist
            let description = format!("**{}**\n{}", t.title, t.artist);

            // Progress bar
            let total_ms = t.duration_ms.unwrap_or(0).max(0);
            let elapsed_ms = state.actual_play_ms();
            let bar = progress_bar(elapsed_ms, total_ms, 20, is_paused);
            let progress_line = format!(
                "{bar}  {}  /  {}",
                format_duration_ms(elapsed_ms),
                format_duration_ms(total_ms)
            );

            // Queue info for footer
            let queue_len = state.meta_queue.len();
            let footer_text = if queue_len >= 2 {
                let next = &state.meta_queue[1];
                format!("Next: {} — {}", next.title, next.artist)
            } else {
                "No tracks queued".to_string()
            };

            // Added-by info
            let added_by_str = if t.source == QueueSource::Radio {
                "🎲 radio".to_string()
            } else if t.added_by.is_empty() {
                String::new()
            } else {
                format!("Added by <@{}>", t.added_by)
            };

            let mut embed = CreateEmbed::new()
                .title(title)
                .description(description)
                .color(color)
                .field("", progress_line, false);

            if !added_by_str.is_empty() {
                embed = embed.field("", added_by_str, false);
            }

            embed = embed.footer(CreateEmbedFooter::new(footer_text));

            embed
        }
        None => serenity::builder::CreateEmbed::new()
            .title("Queue Ended")
            .description("No more tracks in queue.")
            .color(0x0074_7F8D)
            .footer(serenity::builder::CreateEmbedFooter::new(
                "Bot will leave shortly unless a track is queued.",
            )),
    }
}

/// Generates a Unicode progress bar.
#[must_use]
pub fn progress_bar(elapsed_ms: i64, total_ms: i64, width: usize, is_paused: bool) -> String {
    if total_ms <= 0 || width == 0 {
        return "─".repeat(width);
    }
    let ratio = (elapsed_ms as f64 / total_ms as f64).clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let filled = filled.min(width.saturating_sub(1)); // always leave room for cursor
    let cursor = if is_paused { "⏸" } else { "●" };
    format!(
        "{}{}{}",
        "━".repeat(filled),
        cursor,
        "─".repeat(width.saturating_sub(filled + 1))
    )
}

/// Format milliseconds as M:SS (e.g. "2:34"). Returns "--:--" for ≤0.
#[must_use]
pub fn format_duration_ms(ms: i64) -> String {
    if ms <= 0 {
        return "--:--".to_string();
    }
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}

// ── Post Helpers ────────────────────────────────────────────────────────

/// Post or edit the now-playing embed in the text channel.
/// `track = None` renders a "Queue Ended" embed.
pub async fn post_now_playing(
    http: &Http,
    channel_id: ChannelId,
    guild_id: GuildId,
    state_lock: &Arc<Mutex<GuildMusicState>>,
    track: Option<&crate::state::QueuedTrack>,
    existing_msg_id: Option<MessageId>,
) {
    use serenity::builder::{CreateMessage, EditMessage};

    let (embed, is_paused, history_empty) = {
        let state = state_lock.lock().await;
        (
            build_np_embed(track, &state),
            state.is_paused(),
            state.history.is_empty(),
        )
    };

    if let Some(msg_id) = existing_msg_id {
        let edit = EditMessage::new().embed(embed);
        let edit = if track.is_some() {
            edit.components(vec![build_np_action_buttons(
                guild_id,
                is_paused,
                history_empty,
            )])
        } else {
            edit.components(vec![])
        };

        if let Err(e) = edit.execute(http, channel_id.into(), msg_id, None).await {
            warn!(
                guild_id   = %guild_id,
                channel_id = %channel_id,
                error      = %e,
                operation  = "now_playing.edit",
                "failed to edit now-playing message"
            );
        }
    } else {
        let msg = CreateMessage::new().embed(embed);
        let msg = if track.is_some() {
            msg.components(vec![build_np_action_buttons(
                guild_id,
                is_paused,
                history_empty,
            )])
        } else {
            msg
        };
        match msg.execute(http, channel_id.into()).await {
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

/// Post a *new* NP embed (never edits). Returns the message ID if successful.
/// Used on track start to always post a new message (old one left stale per spec).
pub async fn post_now_playing_new(
    http: &Http,
    channel_id: ChannelId,
    guild_id: GuildId,
    state_lock: &Arc<Mutex<GuildMusicState>>,
    track: Option<&QueuedTrack>,
) -> Option<MessageId> {
    use serenity::builder::CreateMessage;

    let (embed, is_paused, history_empty) = {
        let state = state_lock.lock().await;
        (
            build_np_embed(track, &state),
            state.is_paused(),
            state.history.is_empty(),
        )
    };

    let msg = CreateMessage::new().embed(embed);
    let msg = if track.is_some() {
        msg.components(vec![build_np_action_buttons(
            guild_id,
            is_paused,
            history_empty,
        )])
    } else {
        msg
    };
    match msg.execute(http, channel_id.into()).await {
        Ok(sent) => {
            let mut state = state_lock.lock().await;
            state.now_playing_msg = Some(sent.id);
            Some(sent.id)
        }
        Err(e) => {
            warn!(
                guild_id  = %guild_id,
                error     = %e,
                operation = "now_playing.post_new",
                "failed to post now-playing message"
            );
            None
        }
    }
}
