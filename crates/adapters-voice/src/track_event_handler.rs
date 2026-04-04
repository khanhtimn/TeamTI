use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serenity::all::Http;
use serenity::model::id::{ChannelId, GuildId, MessageId};
use songbird::{Event, EventContext, EventHandler as SongbirdEventHandler};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::player::leave_channel;
use crate::state::GuildMusicState;
use crate::state_map::GuildStateMap;

/// Attached to every track's `TrackEvent::End` and `TrackEvent::Error`.
///
/// Songbird's builtin queue handles audio advancement automatically.
/// This handler is responsible only for:
///  1. Popping the finished track from our parallel `meta_queue`.
///  2. Updating the "Now Playing" Discord embed.
///  3. Starting the auto-leave timer when the queue drains.
#[derive(Clone)]
pub struct TrackEventHandler {
    pub guild_id: GuildId,
    pub http: Arc<Http>,
    pub auto_leave_secs: u64,
    pub songbird: Arc<songbird::Songbird>,
    pub state_map: Arc<GuildStateMap>,
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

        // Pop the just-finished track from our metadata mirror.
        // Songbird already popped its own TrackQueue entry via QueueHandler.
        // WI-2: If pop_front returns None, the queue was already empty —
        // /clear or /leave already posted "Queue Ended". Don't double-post.
        state.meta_queue.pop_front()?;

        if state.meta_queue.is_empty() {
            // Queue drained — start the idle/auto-leave timer
            let token = CancellationToken::new();
            state.cancel_auto_leave();
            state.auto_leave_token = Some(token.clone());

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
                    _ = token.cancelled() => {
                        // A new track was enqueued — cancel timer
                    }
                    _ = tokio::time::sleep(Duration::from_secs(secs)) => {
                        let mut state = state_lock.lock().await;
                        state.voice_channel_id = None;
                        state.now_playing_msg  = None;
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
            if let Some(channel_id) = state.text_channel_id {
                let msg_id = state.now_playing_msg;
                drop(state);
                post_now_playing(
                    &self.http,
                    channel_id,
                    self.guild_id,
                    &state_lock,
                    next_track.as_ref(),
                    msg_id,
                )
                .await;
            } else {
                drop(state);
            }
        }

        None
    }
}

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

    let embed = build_now_playing_embed(track, state_lock).await;

    match existing_msg_id {
        Some(msg_id) => {
            let edit = EditMessage::new().embed(embed);
            if let Err(e) = edit.execute(http, channel_id.into(), msg_id, None).await {
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
            let msg = CreateMessage::new().embed(embed);
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
}

async fn build_now_playing_embed<'a>(
    track: Option<&'a crate::state::QueuedTrack>,
    state_lock: &'a Arc<Mutex<GuildMusicState>>,
) -> serenity::builder::CreateEmbed<'a> {
    use serenity::builder::CreateEmbed;

    let state = state_lock.lock().await;
    let queue_len = state.meta_queue.len();
    drop(state);

    match track {
        Some(t) => {
            let duration_str = t
                .duration_ms
                .map(format_duration)
                .unwrap_or_else(|| "Unknown".to_string());

            let description = format!("**{}**\n{}", t.title, t.artist);

            let mut embed = CreateEmbed::new()
                .title("▶  Now Playing")
                .description(description)
                .color(0x1DB954); // Spotify green

            if let Some(ref album) = t.album {
                embed = embed.field("Album", album, true);
            }
            embed = embed.field("Duration", duration_str, true);

            // queue_len includes the current track; remaining = len - 1
            let remaining = queue_len.saturating_sub(1);
            if remaining > 0 {
                embed = embed.field(
                    "Up Next",
                    format!(
                        "{} track{} in queue",
                        remaining,
                        if remaining == 1 { "" } else { "s" }
                    ),
                    false,
                );
            }

            embed
        }
        None => CreateEmbed::new()
            .title("Queue Ended")
            .description("No more tracks in queue.")
            .color(0x747F8D)
            .footer(serenity::builder::CreateEmbedFooter::new(
                "Bot will leave shortly unless a track is queued.",
            )),
    }
}

fn format_duration(ms: i32) -> String {
    let total_secs = ms / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes}:{seconds:02}")
}
