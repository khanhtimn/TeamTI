use serde::{Deserialize, Serialize};

/// Audio analysis status — mirrors enrichment_status pattern.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AnalysisStatus {
    #[default]
    Pending,
    Processing,
    Done,
    Failed,
}

impl std::fmt::Display for AnalysisStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Failed => "failed",
        };
        f.write_str(s)
    }
}

/// Controls the acoustic/taste blend in mood-aware radio.
#[derive(Debug, Clone, Copy)]
pub struct MoodWeight {
    pub acoustic: f32, // 0.0–1.0
    pub taste: f32,    // 0.0–1.0, typically (1.0 - acoustic)
}

impl MoodWeight {
    /// High-energy seed: weight acoustic similarity more.
    pub const ACOUSTIC_DOMINANT: Self = Self {
        acoustic: 0.70,
        taste: 0.30,
    };
    /// Low-energy seed: weight taste/history more.
    pub const TASTE_DOMINANT: Self = Self {
        acoustic: 0.35,
        taste: 0.65,
    };
    /// Balanced blend.
    pub const BALANCED: Self = Self {
        acoustic: 0.50,
        taste: 0.50,
    };
}

impl Default for MoodWeight {
    fn default() -> Self {
        Self::BALANCED
    }
}
