use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum EnrichmentStatus {
    #[default]
    Pending,
    Enriching,
    Done,
    LowConfidence,
    Unmatched,
    Failed,
    Exhausted,
    FileMissing,
}

impl std::fmt::Display for EnrichmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Must match the TEXT values stored in Postgres exactly
        let s = match self {
            Self::Pending => "pending",
            Self::Enriching => "enriching",
            Self::Done => "done",
            Self::LowConfidence => "low_confidence",
            Self::Unmatched => "unmatched",
            Self::Failed => "failed",
            Self::Exhausted => "exhausted",
            Self::FileMissing => "file_missing",
        };
        f.write_str(s)
    }
}
