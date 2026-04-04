use dashmap::DashMap;
use serenity::model::id::GuildId;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::state::GuildMusicState;

/// Per-guild music state, held locally in our handler (no longer uses Serenity TypeMap).
pub type GuildStateMap = DashMap<GuildId, Arc<Mutex<GuildMusicState>>>;
