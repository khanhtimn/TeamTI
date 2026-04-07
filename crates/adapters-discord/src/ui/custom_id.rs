//! Structured serialization/deserialization for Discord component `custom_id`.
//!
//! Replaces raw `format!("queue_skip:{guild_id}:{user_id}")` / `split(':')`
//! patterns with a typed enum (R3 audit fix).
//!
//! Delimiter is `|` — Discord snowflakes are decimal digits only, so `|`
//! can never appear in any field value.

use serenity::model::id::GuildId;

/// Actions triggered by queue embed buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAction {
    PrevPage {
        guild_id: GuildId,
        page: usize,
        user_id: String,
    },
    NextPage {
        guild_id: GuildId,
        page: usize,
        user_id: String,
    },
    Pause {
        guild_id: GuildId,
        user_id: String,
    },
    Skip {
        guild_id: GuildId,
        user_id: String,
    },
    Shuffle {
        guild_id: GuildId,
        user_id: String,
    },
    Clear {
        guild_id: GuildId,
        user_id: String,
    },
}

impl QueueAction {
    /// Serialize to a Discord `custom_id` string (max 100 chars).
    #[must_use]
    pub fn to_custom_id(&self) -> String {
        match self {
            Self::PrevPage {
                guild_id,
                page,
                user_id,
            } => format!("qp|prev|{guild_id}|{page}|{user_id}"),
            Self::NextPage {
                guild_id,
                page,
                user_id,
            } => format!("qp|next|{guild_id}|{page}|{user_id}"),
            Self::Pause { guild_id, user_id } => format!("qa|pause|{guild_id}|{user_id}"),
            Self::Skip { guild_id, user_id } => format!("qa|skip|{guild_id}|{user_id}"),
            Self::Shuffle { guild_id, user_id } => format!("qa|shuffle|{guild_id}|{user_id}"),
            Self::Clear { guild_id, user_id } => format!("qa|clear|{guild_id}|{user_id}"),
        }
    }

    /// Parse a Discord `custom_id` string back into a `QueueAction`.
    /// Returns `None` for unrecognized or malformed IDs.
    #[must_use]
    pub fn from_custom_id(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('|').collect();
        match parts.as_slice() {
            ["qp", "prev", g, p, u] => Some(Self::PrevPage {
                guild_id: GuildId::new(g.parse().ok()?),
                page: p.parse().ok()?,
                user_id: u.to_string(),
            }),
            ["qp", "next", g, p, u] => Some(Self::NextPage {
                guild_id: GuildId::new(g.parse().ok()?),
                page: p.parse().ok()?,
                user_id: u.to_string(),
            }),
            ["qa", "pause", g, u] => Some(Self::Pause {
                guild_id: GuildId::new(g.parse().ok()?),
                user_id: u.to_string(),
            }),
            ["qa", "skip", g, u] => Some(Self::Skip {
                guild_id: GuildId::new(g.parse().ok()?),
                user_id: u.to_string(),
            }),
            ["qa", "shuffle", g, u] => Some(Self::Shuffle {
                guild_id: GuildId::new(g.parse().ok()?),
                user_id: u.to_string(),
            }),
            ["qa", "clear", g, u] => Some(Self::Clear {
                guild_id: GuildId::new(g.parse().ok()?),
                user_id: u.to_string(),
            }),
            _ => None,
        }
    }

    /// Returns `true` if this is a queue-related action (starts with `qp|` or `qa|`).
    #[must_use]
    pub fn is_queue_action(custom_id: &str) -> bool {
        custom_id.starts_with("qp|") || custom_id.starts_with("qa|")
    }
}

/// Actions triggered by Now Playing embed buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NPAction {
    Pause { guild_id: GuildId },
    Skip { guild_id: GuildId },
}

impl NPAction {
    #[must_use]
    pub fn to_custom_id(&self) -> String {
        match self {
            Self::Pause { guild_id } => format!("np|pause|{guild_id}"),
            Self::Skip { guild_id } => format!("np|skip|{guild_id}"),
        }
    }

    #[must_use]
    pub fn from_custom_id(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('|').collect();
        match parts.as_slice() {
            ["np", "pause", g] => Some(Self::Pause {
                guild_id: GuildId::new(g.parse().ok()?),
            }),
            ["np", "skip", g] => Some(Self::Skip {
                guild_id: GuildId::new(g.parse().ok()?),
            }),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_np_action(custom_id: &str) -> bool {
        custom_id.starts_with("np|")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_prev_page() {
        let action = QueueAction::PrevPage {
            guild_id: GuildId::new(123456),
            page: 2,
            user_id: "789".to_string(),
        };
        let id = action.to_custom_id();
        assert_eq!(id, "qp|prev|123456|2|789");
        assert_eq!(QueueAction::from_custom_id(&id), Some(action));
    }

    #[test]
    fn roundtrip_action_buttons() {
        let action = QueueAction::Pause {
            guild_id: GuildId::new(111),
            user_id: "222".to_string(),
        };
        let id = action.to_custom_id();
        assert_eq!(id, "qa|pause|111|222");
        assert_eq!(QueueAction::from_custom_id(&id), Some(action));
    }

    #[test]
    fn malformed_returns_none() {
        assert_eq!(QueueAction::from_custom_id("garbage"), None);
        assert_eq!(QueueAction::from_custom_id("qp|prev|notanumber|0|0"), None);
        assert_eq!(QueueAction::from_custom_id(""), None);
    }

    #[test]
    fn is_queue_action_check() {
        assert!(QueueAction::is_queue_action("qp|prev|123|0|456"));
        assert!(QueueAction::is_queue_action("qa|skip|123|456"));
        assert!(!QueueAction::is_queue_action("queue_skip:123:456"));
        assert!(!QueueAction::is_queue_action("fav_page:abc"));
    }
}
