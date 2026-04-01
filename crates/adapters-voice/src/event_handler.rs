use async_trait::async_trait;
use songbird::events::{Event, EventContext, EventHandler};
use tracing::{debug, error, info, warn};

/// Logs all songbird driver and track lifecycle events.
///
/// Registered via `Call::add_global_event()` during voice join.
/// In Phase 2, this can be replaced with a handler that pushes
/// structured events into an MPSC channel for the application layer.
pub struct SongbirdEventLogger;

#[async_trait]
impl EventHandler for SongbirdEventLogger {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::DriverConnect(data) => {
                info!(
                    channel_id = ?data.channel_id,
                    guild_id = ?data.guild_id,
                    server = %data.server,
                    ssrc = data.ssrc,
                    "Voice driver CONNECTED"
                );
            }
            EventContext::DriverReconnect(data) => {
                warn!(
                    channel_id = ?data.channel_id,
                    guild_id = ?data.guild_id,
                    server = %data.server,
                    "Voice driver RECONNECTED"
                );
            }
            EventContext::DriverDisconnect(data) => {
                error!(
                    kind = ?data.kind,
                    reason = ?data.reason,
                    channel_id = ?data.channel_id,
                    guild_id = ?data.guild_id,
                    "Voice driver DISCONNECTED"
                );
            }
            EventContext::Track(track_list) => {
                for (state, handle) in *track_list {
                    debug!(
                        track_id = ?handle.uuid(),
                        playing = ?state.playing,
                        volume = %state.volume,
                        "Track event fired"
                    );
                }
            }
            _ => {}
        }
        None // Keep handler active
    }
}
