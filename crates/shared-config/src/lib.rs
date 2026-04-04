use std::path::PathBuf;

#[derive(Debug)]
pub struct ConfigError {
    pub message: String,
}

impl ConfigError {
    #[must_use]
    pub fn missing(key: &str) -> Self {
        Self {
            message: format!("Required environment variable {key} is not set"),
        }
    }

    pub fn parse(key: &str, err: impl std::fmt::Display) -> Self {
        Self {
            message: format!("Failed to parse environment variable {key}: {err}"),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

pub struct Config {
    // --- existing v1 fields ---
    pub database_url: String,
    pub discord_token: String,
    pub discord_guild_id: u64,

    /// Seconds to wait after queue empty before leaving voice channel.
    /// Default: 30
    pub auto_leave_secs: u64,

    // --- v2 fields ---
    /// Absolute path to SMB-mounted music library root.
    pub media_root: PathBuf,

    /// AcoustID API key.
    pub acoustid_api_key: String,

    /// MusicBrainz User-Agent header.
    /// Required format: "AppName/Version (contact@email.com)"
    pub mb_user_agent: String,

    /// PollWatcher poll interval in seconds. Default: 300.
    pub scan_interval_secs: u64,

    /// SMB_READ_SEMAPHORE permit count. Default: 3.
    pub smb_read_concurrency: usize,

    /// Max concurrent Fingerprint Workers. Default: 4.
    pub fingerprint_concurrency: usize,

    /// Max concurrent Cover Art Archive fetches. Default: 4.
    pub cover_art_concurrency: usize,

    /// AcoustID minimum confidence score for 'done'. Default: 0.85.
    pub enrichment_confidence_threshold: f32,

    /// AcoustID no-match retries before 'exhausted'. Default: 3.
    pub unmatched_retry_limit: i32,

    /// Network-error retries before 'exhausted'. Default: 5.
    pub failed_retry_limit: i32,

    /// SQLx pool max connections. Default: 10.
    pub db_pool_size: u32,

    /// Maximum concurrent tag write operations. Default: 2.
    /// Each operation loads the full audio file into memory.
    /// Lower this if memory pressure is observed during bulk tag writeback.
    pub tag_write_concurrency: usize,

    /// Whether to fetch composer/lyricist from MusicBrainz Work entities.
    /// Doubles MB API calls per enrichment (~0.5 tracks/sec). Default: true.
    pub mb_fetch_work_credits: bool,
}

impl Config {
    /// Load configuration from environment variables.
    /// Kept as `load()` for backward compatibility with v1 call sites.
    #[must_use]
    pub fn load() -> Self {
        Self::from_env().expect("Failed to load config from environment")
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok(); // load .env if present; silently ignore if absent
        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .map_err(|_| ConfigError::missing("DATABASE_URL"))?,
            discord_token: std::env::var("DISCORD_TOKEN")
                .map_err(|_| ConfigError::missing("DISCORD_TOKEN"))?,
            discord_guild_id: std::env::var("DISCORD_GUILD_ID")
                .map_err(|_| ConfigError::missing("DISCORD_GUILD_ID"))?
                .parse()
                .map_err(|e| ConfigError::parse("DISCORD_GUILD_ID", e))?,
            media_root: std::env::var("MEDIA_ROOT")
                .map_or_else(|_| PathBuf::from("./media_data"), PathBuf::from),
            acoustid_api_key: std::env::var("ACOUSTID_API_KEY").unwrap_or_default(),
            mb_user_agent: std::env::var("MB_USER_AGENT")
                .unwrap_or_else(|_| "TeamTI/0.1.0 (teamti@localhost)".to_string()),
            scan_interval_secs: parse_env("SCAN_INTERVAL_SECS", 300)?,
            smb_read_concurrency: parse_env("SMB_READ_CONCURRENCY", 3)?,
            fingerprint_concurrency: parse_env("FINGERPRINT_CONCURRENCY", 4)?,
            cover_art_concurrency: parse_env("COVER_ART_CONCURRENCY", 4)?,
            enrichment_confidence_threshold: parse_env("ENRICHMENT_CONFIDENCE_THRESHOLD", 0.85f32)?,
            unmatched_retry_limit: parse_env("UNMATCHED_RETRY_LIMIT", 3i32)?,
            failed_retry_limit: parse_env("FAILED_RETRY_LIMIT", 5i32)?,
            db_pool_size: parse_env("DB_POOL_SIZE", 10u32)?,
            tag_write_concurrency: parse_env("TAG_WRITE_CONCURRENCY", 2usize)?,
            auto_leave_secs: parse_env("AUTO_LEAVE_SECS", 30u64)?,
            mb_fetch_work_credits: std::env::var("MB_FETCH_WORK_CREDITS")
                .map_or(true, |v| v != "0" && v.to_lowercase() != "false"),
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.mb_user_agent.contains('/') || !self.mb_user_agent.contains('(') {
            return Err(ConfigError::parse(
                "MB_USER_AGENT",
                "Must follow format 'AppName/Version (Contact)'",
            ));
        }
        Ok(())
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(val) => val.parse::<T>().map_err(|e| ConfigError::parse(key, e)),
        Err(_) => Ok(default),
    }
}
