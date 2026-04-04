use std::collections::VecDeque;

use serenity::model::id::{ChannelId, MessageId};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use domain::track::{Track, TrackSummary};

/// Lightweight metadata for a queued track.
/// Kept in a parallel `VecDeque` alongside Songbird's `TrackQueue`.
/// Songbird owns playback; we own the display metadata.
#[derive(Debug, Clone)]
pub struct QueuedTrack {
    pub track_id: Uuid,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<i32>,
    pub blob_location: String,
}

impl From<&Track> for QueuedTrack {
    fn from(t: &Track) -> Self {
        Self {
            track_id: t.id,
            title: t.title.clone(),
            artist: t.artist_display.clone().unwrap_or_default(),
            album: None,
            duration_ms: t.duration_ms,
            blob_location: t.blob_location.clone(),
        }
    }
}

impl From<TrackSummary> for QueuedTrack {
    fn from(s: TrackSummary) -> Self {
        Self {
            track_id: s.id,
            title: s.title,
            artist: s.artist_display.unwrap_or_default(),
            album: None,
            duration_ms: s.duration_ms,
            blob_location: s.blob_location,
        }
    }
}

/// Per-guild state managed alongside Songbird's `Call`.
///
/// `meta_queue` is a parallel `VecDeque<QueuedTrack>` that mirrors
/// what Songbird's `TrackQueue` is playing. Elements are pushed when
/// we call `enqueue_track()` and popped by `TrackEventHandler` when
/// a track ends (after Songbird fires `TrackEvent::End`).
pub struct GuildMusicState {
    /// Mirrors Songbird's TrackQueue — metadata only, no audio handle.
    pub meta_queue: VecDeque<QueuedTrack>,

    // Voice
    pub voice_channel_id: Option<ChannelId>,

    // Text — channel where now-playing messages are sent
    pub text_channel_id: Option<ChannelId>,
    pub now_playing_msg: Option<MessageId>,

    // Auto-leave: cancelled when a new track is queued
    pub auto_leave_token: Option<CancellationToken>,
}

impl Default for GuildMusicState {
    fn default() -> Self {
        Self::new()
    }
}

impl GuildMusicState {
    pub fn new() -> Self {
        Self {
            meta_queue: VecDeque::new(),
            voice_channel_id: None,
            text_channel_id: None,
            now_playing_msg: None,
            auto_leave_token: None,
        }
    }

    /// Cancel any running auto-leave timer.
    pub fn cancel_auto_leave(&mut self) {
        if let Some(token) = self.auto_leave_token.take() {
            token.cancel();
        }
    }

    /// True when the metadata queue is empty (Songbird queue is also empty).
    pub fn is_idle(&self) -> bool {
        self.meta_queue.is_empty()
    }
}
