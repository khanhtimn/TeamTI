use std::collections::VecDeque;
use std::time::Instant;

use serenity::model::id::{ChannelId, MessageId};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use domain::track::{Track, TrackSummary};

/// How a track ended up in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueSource {
    /// Added by a user via /play or /playlist play.
    Manual,
    /// Added silently by the radio refill engine.
    Radio,
}

/// Lightweight metadata for a queued track.
/// Kept in a parallel `VecDeque` alongside Songbird's `TrackQueue`.
/// Songbird owns playback; we own the display metadata.
#[derive(Debug, Clone)]
pub struct QueuedTrack {
    pub track_id: Uuid,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Duration from DB — `Option<i64>` mirrors `tracks.duration_ms BIGINT`.
    pub duration_ms: Option<i64>,
    pub blob_location: String,
    /// Discord user ID (snowflake as String) of whoever added this track.
    pub added_by: String,
    /// Whether this was manually queued or added by radio.
    pub source: QueueSource,
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
            added_by: String::new(),
            source: QueueSource::Manual,
        }
    }
}

impl From<TrackSummary> for QueuedTrack {
    fn from(s: TrackSummary) -> Self {
        Self {
            track_id: s.id,
            title: s.title,
            artist: s.artist_display.unwrap_or_default(),
            album: s.album_title,
            duration_ms: s.duration_ms,
            blob_location: s.blob_location.unwrap_or_default(),
            added_by: String::new(),
            source: QueueSource::Manual,
        }
    }
}

/// Per-guild state managed alongside Songbird's `Call`.
///
/// `meta_queue` is a parallel `VecDeque<QueuedTrack>` that mirrors
/// what Songbird's `TrackQueue` is playing. Elements are pushed when
/// we call `enqueue_track()` and popped by `TrackEventHandler` when
/// a track ends (after Songbird fires `TrackEvent::End`).
///
/// # Duration type convention
///
/// All millisecond durations in this struct use `i64`.  This avoids
/// casts between the DB type (`i32`), the Rust clock type (`u128`),
/// and display code.  `i64` holds any realistic duration (up to ~292M
/// years) and subsumes `i32` without casting.  The only narrowing
/// point is `as i64` from the system clock (`u128`).
/// to `i32::MAX`.
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

    // ── Pass 3: Radio mode ────────────────────────────────────────
    /// Whether radio mode is active for this guild.
    pub radio_mode: bool,
    /// The user who started radio mode (for recommendation seeding).
    pub radio_user_id: Option<String>,

    // ── Pass 5: Pause duration tracking ───────────────────────────
    /// When the current (front) track started playing.
    /// Reset to `Some(Instant::now())` on TrackStarted.
    pub track_started_at: Option<Instant>,

    /// When the current track was paused. None when not paused.
    pub paused_at: Option<Instant>,

    /// Accumulated paused duration for the current track (milliseconds).
    /// `i64` to match the universal ms type convention.
    pub total_paused_ms: i64,

    // ── Pass 5: NP auto-update ────────────────────────────────────
    /// Cancellation token for the NP auto-update background task.
    /// Cancelled when a new track starts or the bot leaves.
    ///
    /// On startup/restart, this is always `None`. Any NP message from a
    /// previous session is left stale in Discord — intentional per UX
    /// decision B3. The bot posts a new NP message on the first
    /// `TrackStarted` event after restart.
    pub np_update_cancel: Option<CancellationToken>,
}

impl Default for GuildMusicState {
    fn default() -> Self {
        Self::new()
    }
}

impl GuildMusicState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta_queue: VecDeque::new(),
            voice_channel_id: None,
            text_channel_id: None,
            now_playing_msg: None,
            auto_leave_token: None,
            radio_mode: false,
            radio_user_id: None,
            track_started_at: None,
            paused_at: None,
            total_paused_ms: 0,
            np_update_cancel: None,
        }
    }

    /// Cancel any running auto-leave timer.
    pub fn cancel_auto_leave(&mut self) {
        if let Some(token) = self.auto_leave_token.take() {
            token.cancel();
        }
    }

    /// Cancel the NP auto-update background task.
    pub fn cancel_np_update(&mut self) {
        if let Some(token) = self.np_update_cancel.take() {
            token.cancel();
        }
    }

    /// Full cleanup on disconnect — used by both `/leave` and the
    /// auto-leave timer to ensure identical teardown.
    pub fn cleanup_on_leave(&mut self) {
        self.cancel_np_update();
        self.cancel_auto_leave();
        self.meta_queue.clear();
        self.voice_channel_id = None;
        self.now_playing_msg = None;
        self.track_started_at = None;
        self.paused_at = None;
        self.total_paused_ms = 0;
        self.radio_mode = false;
        self.radio_user_id = None;
        // text_channel_id intentionally preserved — so a /play after
        // rejoin posts NP to the same channel
    }

    /// True when the metadata queue is empty (Songbird queue is also empty).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.meta_queue.is_empty()
    }

    /// Whether the current track is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Compute actual play duration (excluding paused time) in ms.
    /// Returns `i64` — the universal ms type.
    #[must_use]
    pub fn actual_play_ms(&self) -> i64 {
        let elapsed = self
            .track_started_at
            .map_or(0, |s| s.elapsed().as_millis() as i64);
        (elapsed - self.total_paused_ms).max(0)
    }

    /// Reset pause/play tracking for a new track.
    pub fn reset_track_timing(&mut self) {
        self.track_started_at = Some(Instant::now());
        self.paused_at = None;
        self.total_paused_ms = 0;
    }
}
