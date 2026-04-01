#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GuildId(pub u64);

impl From<u64> for GuildId {
    fn from(val: u64) -> Self {
        GuildId(val)
    }
}
