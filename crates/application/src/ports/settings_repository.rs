use async_trait::async_trait;
use domain::guild::GuildId;
use domain::error::DomainError;

#[async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn get_prefix(&self, guild_id: GuildId) -> Result<Option<String>, DomainError>;
}
