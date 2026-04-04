use std::path::Path;
use std::sync::Arc;

use serenity::model::id::{ChannelId, GuildId};
use songbird::input::File as SongbirdFile;
use songbird::tracks::TrackHandle;
use songbird::{Event, TrackEvent};

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
pub async fn enqueue_track(
    songbird: &Arc<songbird::Songbird>,
    guild_id: GuildId,
    track: &QueuedTrack,
    media_root: &Path,
    http: &Arc<serenity::all::Http>,
    auto_leave_secs: u64,
    state_map: &Arc<GuildStateMap>,
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

    // enqueue_input adds to Songbird's TrackQueue and auto-plays when the
    // queue is empty.
    let handle = {
        let mut handler = handler_lock.lock().await;
        handler.enqueue_input(source.into()).await
    };

    // Attach our metadata/side-effect handler to both End and Error events.
    // This keeps audio advancement with Songbird while we handle embeds + auto-leave.
    let event_handler = TrackEventHandler {
        guild_id,
        http: Arc::clone(http),
        auto_leave_secs,
        songbird: Arc::clone(songbird),
        state_map: Arc::clone(state_map),
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
