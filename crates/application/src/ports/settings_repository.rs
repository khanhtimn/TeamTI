use async_trait::async_trait;
use domain::error::DomainError;
use domain::guild::GuildId;

#[async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_prefix(&self, guild_id: GuildId) -> Result<Option<String>, DomainError>;
}
