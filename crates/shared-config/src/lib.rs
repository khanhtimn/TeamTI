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
    pub user_agent: String,

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

    /// Absolute path to the Tantivy index directory.
    ///
    /// MUST be on local disk. Do NOT point this at the NAS SMB mount.
    /// Memory-mapped files over a network filesystem cause undefined behavior
    /// on network interruption. The index is fully reconstructable from
    /// PostgreSQL in ~2 seconds.
    ///
    /// Absolute path to the Tantivy index directory.
    pub tantivy_index_path: PathBuf,

    // --- Pass 4 fields ---
    /// Last.fm API key for similar-artist lookups. Optional.
    pub lastfm_api_key: Option<String>,

    /// Max concurrent bliss-audio analysis tasks. Default: 4.
    pub analysis_concurrency: usize,

    /// Seconds between analysis worker poll cycles. Default: 30.
    pub analysis_poll_secs: u64,

    // --- v4 fields: YouTube ---
    /// Path to the yt-dlp binary. Default: "yt-dlp" (uses $PATH).
    pub ytdlp_binary: String,

    /// Optional: path to Netscape-format cookies file for bot detection bypass.
    pub ytdlp_cookies_file: Option<String>,

    /// Max simultaneous yt-dlp download processes. Default: 2.
    pub ytdlp_download_concurrency: usize,

    /// Number of tracks ahead in queue to pre-download (just-in-time). Default: 3.
    pub ytdlp_lookahead_depth: usize,

    /// Max download attempts before permanent failure. Default: 5.
    pub ytdlp_max_download_attempts: u32,
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
            user_agent: std::env::var("USER_AGENT")
                .or_else(|_| std::env::var("MB_USER_AGENT")) // Fallback for transition
                .unwrap_or_else(|_| "TeamTI/0.1.0 (local)".to_string()),
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
            tantivy_index_path: std::env::var("TANTIVY_INDEX_PATH")
                .map_or_else(|_| PathBuf::from("./search_index"), PathBuf::from),
            lastfm_api_key: std::env::var("LASTFM_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            analysis_concurrency: parse_env("ANALYSIS_CONCURRENCY", 4usize)?,
            analysis_poll_secs: parse_env("ANALYSIS_POLL_SECS", 30u64)?,
            ytdlp_binary: std::env::var("YTDLP_BINARY").unwrap_or_else(|_| "yt-dlp".to_string()),
            ytdlp_cookies_file: std::env::var("YTDLP_COOKIES_FILE")
                .ok()
                .filter(|s| !s.is_empty()),
            ytdlp_download_concurrency: parse_env("YTDLP_DOWNLOAD_CONCURRENCY", 2usize)?,
            ytdlp_lookahead_depth: parse_env("YTDLP_LOOKAHEAD_DEPTH", 3usize)?,
            ytdlp_max_download_attempts: parse_env("YTDLP_MAX_DOWNLOAD_ATTEMPTS", 5u32)?,
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.user_agent.contains('/') || !self.user_agent.contains('(') {
            return Err(ConfigError::parse(
                "USER_AGENT",
                format!(
                    "USER_AGENT must be in format 'AppName/version (contact-url)'. Current: {}",
                    self.user_agent
                ),
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
