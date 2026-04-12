use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serenity::model::id::{ChannelId, GuildId};
use songbird::input::File as SongbirdFile;
use songbird::input::YoutubeDl;
use songbird::input::{AudioStream, AudioStreamError, Compose};
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
///
/// Supports two input modes:
/// - **Local/cached**: `blob_location` is `Some` → play from local file via `SongbirdFile`
/// - **YouTube streaming**: `blob_location` is `None` → stream via `YoutubeDl`
use async_trait::async_trait;

struct LazyHybridInput {
    media_root: PathBuf,
    expected_blob_path: Option<String>,
    /// Video ID for fallback file discovery when expected blob path is stale (F3 fix).
    video_id: String,
    ytdlp_binary: String,
    client: reqwest::Client,
    page_url: String,
}

#[async_trait]
impl Compose for LazyHybridInput {
    fn create(
        &mut self,
    ) -> Result<AudioStream<Box<dyn symphonia_core::io::MediaSource>>, AudioStreamError> {
        unimplemented!()
    }

    async fn create_async(
        &mut self,
    ) -> Result<AudioStream<Box<dyn symphonia_core::io::MediaSource>>, AudioStreamError> {
        // If we have an expected blob path, check if it exists on disk now!
        if let Some(ref expected_blob) = self.expected_blob_path {
            let abs_path = self.media_root.join(expected_blob);
            if abs_path.exists() {
                tracing::info!(path = %abs_path.display(), "Mid-queue reroute: using local file instead of Youtube stream");
                let mut source = SongbirdFile::new(abs_path);
                return source.create_async().await;
            }
        }

        // F3 fix: If expected blob path was stale (flat-playlist repair changed it),
        // scan the youtube directory for any file ending in _{video_id}.m4a
        if !self.video_id.is_empty() {
            let youtube_dir = self.media_root.join("youtube");
            if let Some(found) = find_file_by_video_id(&youtube_dir, &self.video_id) {
                tracing::info!(path = %found.display(), video_id = %self.video_id, "F3 recovery: found file via video_id scan");
                let mut source = SongbirdFile::new(found);
                return source.create_async().await;
            }
        }

        // Fall back to live streaming
        tracing::info!(url = %self.page_url, "Using YouTube stream (file not yet downloaded)");
        let mut ytdl = YoutubeDl::new_ytdl_like(
            &self.ytdlp_binary,
            self.client.clone(),
            self.page_url.clone(),
        );
        ytdl.create_async().await
    }

    fn should_create_async(&self) -> bool {
        true
    }
}

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
    ytdlp_binary: &str,
) -> Result<TrackHandle, AppError> {
    let handler_lock = songbird.get(guild_id).ok_or_else(|| AppError::Voice {
        kind: VoiceErrorKind::NotInChannel,
        detail: "not in a voice channel".to_string(),
    })?;

    // ── Build the Songbird Input based on whether we have a local file ────
    let handle = if let Some(ref blob_loc) = track.blob_location {
        // Cached / local file path
        let abs_path = media_root.join(blob_loc);
        if !abs_path.exists() {
            return Err(AppError::Voice {
                kind: VoiceErrorKind::FileNotFound,
                detail: abs_path.display().to_string(),
            });
        }

        let source = SongbirdFile::new(abs_path);
        let mut handler = handler_lock.lock().await;
        handler.enqueue_input(source.into()).await
    } else if let Some(video_id) = track.youtube_video_id.as_deref() {
        let page_url = format!("https://www.youtube.com/watch?v={video_id}");

        let client = reqwest::Client::new();

        let mut handler = handler_lock.lock().await;
        // O4 Fix: Use LazyHybridInput to check if the file finishes downloading mid-queue
        let lazy_source = LazyHybridInput {
            media_root: media_root.to_path_buf(),
            expected_blob_path: track.youtube_blob_path.clone(),
            video_id: video_id.to_string(),
            ytdlp_binary: ytdlp_binary.to_string(),
            client,
            page_url,
        };
        handler
            .enqueue_input(songbird::input::Input::Lazy(Box::new(lazy_source)))
            .await
    } else {
        return Err(AppError::Voice {
            kind: VoiceErrorKind::FileNotFound,
            detail: format!(
                "track {} has no blob_location and no stream_url",
                track.track_id
            ),
        });
    };

    // Record track start time in guild state and emit TrackStarted for the first track
    let mut emit_start = false;
    let mut users_in_channel = Vec::new();
    if let Some(state_entry) = state_map.get(&guild_id) {
        let mut state = state_entry.lock().await;

        // Immediately map the native handle UUID back to the meta_queue track securely
        for queued in state.meta_queue.iter_mut().rev() {
            if queued.track_id == track.track_id && queued.songbird_uuid.is_none() {
                queued.songbird_uuid = Some(handle.uuid());
                break;
            }
        }

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

    let path_display = track.blob_location.as_deref().unwrap_or("[youtube-stream]");
    tracing::info!(
        guild_id   = %guild_id,
        track_id   = %track.track_id,
        path       = %path_display,
        operation  = "voice.enqueue",
        "enqueued track to Songbird builtin queue"
    );

    Ok(handle)
}

/// F3 recovery: scan `youtube/` subdirectories for a file whose name
/// ends with `_{video_id}.m4a`. This handles the case where the download
/// worker repaired a flat-playlist stub and the blob_path changed after
/// the track was already queued.
fn find_file_by_video_id(youtube_dir: &Path, video_id: &str) -> Option<PathBuf> {
    let suffix = format!("_{video_id}.m4a");
    let entries = std::fs::read_dir(youtube_dir).ok()?;
    for uploader_entry in entries.flatten() {
        let uploader_path = uploader_entry.path();
        if !uploader_path.is_dir() {
            continue;
        }
        let files = std::fs::read_dir(&uploader_path).ok()?;
        for file_entry in files.flatten() {
            let file_path = file_entry.path();
            if file_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
            {
                return Some(file_path);
            }
        }
    }
    None
}
