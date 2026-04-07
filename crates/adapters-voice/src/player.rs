use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serenity::model::id::{ChannelId, GuildId};
use songbird::input::File as SongbirdFile;
use songbird::tracks::TrackHandle;
use songbird::{Event, TrackEvent};

use crate::lifecycle::TrackLifecycleTx;
use crate::state::QueuedTrack;
use crate::state_map::GuildStateMap;
use crate::track_event_handler::TrackEventHandler;
use application::error::{AppError, VoiceErrorKind};

/// Join the specified voice channel. If already in a different channel,
/// leave first. Returns error if unable to connect.
pub async fn join_channel(
    songbird: &Arc<songbird::Songbird>,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Result<(), AppError> {
    // join() automatically leaves the previous channel if in one
    let _call_lock = songbird
        .join(guild_id, channel_id)
        .await
        .map_err(|e| AppError::Voice {
            kind: VoiceErrorKind::JoinFailed,
            detail: e.to_string(),
        })?;

    tracing::info!(
        guild_id   = %guild_id,
        channel_id = %channel_id,
        operation  = "voice.join",
        "joined voice channel"
    );
    Ok(())
}

/// Leave the voice channel for the guild.
/// Stops and clears Songbird's builtin queue before leaving.
pub async fn leave_channel(
    songbird: &Arc<songbird::Songbird>,
    guild_id: GuildId,
) -> Result<(), AppError> {
    // Clear the Songbird queue so no lingering track events fire after leave
    if let Some(handler_lock) = songbird.get(guild_id) {
        handler_lock.lock().await.queue().stop();
    }

    songbird
        .leave(guild_id)
        .await
        .map_err(|e| AppError::Voice {
            kind: VoiceErrorKind::NotInChannel,
            detail: e.to_string(),
        })?;

    tracing::info!(
        guild_id  = %guild_id,
        operation = "voice.leave",
        "left voice channel"
    );
    Ok(())
}

/// Enqueue a track into Songbird's builtin queue.
/// Attaches a `TrackEventHandler` for metadata sync, "Now Playing" messages,
/// and auto-leave on idle.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_track(
    songbird: &Arc<songbird::Songbird>,
    guild_id: GuildId,
    track: &QueuedTrack,
    media_root: &Path,
    http: &Arc<serenity::all::Http>,
    cache: &Arc<serenity::all::Cache>,
    auto_leave_secs: u64,
    state_map: &Arc<GuildStateMap>,
    lifecycle_tx: TrackLifecycleTx,
) -> Result<TrackHandle, AppError> {
    let handler_lock = songbird.get(guild_id).ok_or_else(|| AppError::Voice {
        kind: VoiceErrorKind::NotInChannel,
        detail: "not in a voice channel".to_string(),
    })?;

    let abs_path = media_root.join(&track.blob_location);
    if !abs_path.exists() {
        return Err(AppError::Voice {
            kind: VoiceErrorKind::FileNotFound,
            detail: abs_path.display().to_string(),
        });
    }

    let source = SongbirdFile::new(abs_path);

    let handle = {
        let mut handler = handler_lock.lock().await;
        handler.enqueue_input(source.into()).await
    };

    // Record track start time in guild state and emit TrackStarted for the first track
    let mut emit_start = false;
    let mut users_in_channel = Vec::new();
    if let Some(state_entry) = state_map.get(&guild_id) {
        let mut state = state_entry.lock().await;
        // Only set if this is the first/only track (i.e. it starts playing immediately)
        if state.meta_queue.len() <= 1 {
            state.track_started_at = Some(Instant::now());
            emit_start = true;
            if let Some(channel_id) = state.voice_channel_id {
                let cache_clone = Arc::clone(cache);
                users_in_channel = cache_clone
                    .guild(guild_id)
                    .map(|g| {
                        g.voice_states
                            .iter()
                            .filter(|vs| vs.channel_id == Some(channel_id))
                            .filter(|vs| !g.members.get(&vs.user_id).is_some_and(|m| m.user.bot()))
                            .map(|vs| vs.user_id.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
    }

    if emit_start {
        let _ = lifecycle_tx.send(crate::lifecycle::TrackLifecycleEvent::TrackStarted {
            guild_id,
            track_id: track.track_id,
            track_duration_ms: track.duration_ms,
            users_in_channel,
        });
    }

    // Attach our metadata/side-effect handler to both End and Error events.
    // This keeps audio advancement with Songbird while we handle embeds + auto-leave.
    let event_handler = TrackEventHandler {
        guild_id,
        http: Arc::clone(http),
        cache: Arc::clone(cache),
        auto_leave_secs,
        songbird: Arc::clone(songbird),
        state_map: Arc::clone(state_map),
        lifecycle_tx: lifecycle_tx.clone(),
    };

    handle
        .add_event(Event::Track(TrackEvent::End), event_handler.clone())
        .ok();
    handle
        .add_event(Event::Track(TrackEvent::Error), event_handler)
        .ok();

    tracing::info!(
        guild_id   = %guild_id,
        track_id   = %track.track_id,
        path       = %track.blob_location,
        operation  = "voice.enqueue",
        "enqueued track to Songbird builtin queue"
    );

    Ok(handle)
}
