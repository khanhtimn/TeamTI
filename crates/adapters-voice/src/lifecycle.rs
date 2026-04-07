//! Lightweight lifecycle events emitted by the voice adapter.
//!
//! The voice adapter doesn't import any application ports.
//! Instead, it emits these events on an `UnboundedSender` and a consumer
//! task in the bot/discord layer handles the business logic (opening/closing
//! listen events, triggering radio refills).

use serenity::model::id::GuildId;
use uuid::Uuid;

/// Events emitted by the voice adapter when track lifecycle changes occur.
#[derive(Debug, Clone)]
pub enum TrackLifecycleEvent {
    /// A track started playing. The Discord layer should open listen events
    /// for each user in `users_in_channel`.
    TrackStarted {
        guild_id: GuildId,
        track_id: Uuid,
        track_duration_ms: Option<i64>,
        users_in_channel: Vec<String>,
    },
    /// A track ended (finished, skipped, or errored). The Discord layer
    /// should close all open listen events for this track.
    TrackEnded {
        guild_id: GuildId,
        track_id: Uuid,
        track_duration_ms: Option<i64>,
        play_duration_ms: i64,
    },
    /// Radio mode queue is running low. The Discord layer should call
    /// `RecommendationPort::recommend()` and enqueue results.
    RadioRefillNeeded {
        guild_id: GuildId,
        user_id: String,
        seed_track_id: Option<Uuid>,
    },
    /// Pass 4: A listen event completed — the lifecycle worker should
    /// refresh the user's affinities and update genre/guild stats.
    AffinityUpdate {
        guild_id: GuildId,
        user_id: String,
        track_id: Uuid,
    },
}

/// Sender half of the lifecycle event channel.
/// UnboundedSender is used here because lifecycle events are low-volume
/// (one per track start/end) and the worker processes them quickly.
/// If this bot scales to many concurrent guilds or the DB becomes a
/// bottleneck, switch to mpsc::channel with a bounded buffer and add
/// backpressure handling. TODO: revisit in v4 server architecture.
pub type TrackLifecycleTx = tokio::sync::mpsc::UnboundedSender<TrackLifecycleEvent>;

/// Receiver half of the lifecycle event channel.
pub type TrackLifecycleRx = tokio::sync::mpsc::UnboundedReceiver<TrackLifecycleEvent>;
