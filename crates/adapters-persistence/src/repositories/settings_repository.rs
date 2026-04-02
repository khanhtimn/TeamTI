use crate::db::Database;
use application::ports::settings_repository::SettingsRepository;
use async_trait::async_trait;
use domain::error::DomainError;
use domain::guild::GuildId;

pub struct PgSettingsRepository {
    db: Database,
}

impl PgSettingsRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SettingsRepository for PgSettingsRepository {
    async fn get_prefix(&self, _guild_id: GuildId) -> Result<Option<String>, DomainError> {
        // Just stubbing for v1. Real implementation would fetch from DB.
        Ok(None)
    }
}
