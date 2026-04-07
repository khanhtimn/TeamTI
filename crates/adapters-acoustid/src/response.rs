use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AcoustIdResponse {
    pub status: String,
    #[serde(default)]
    pub results: Vec<AcoustIdResult>,
}

#[derive(Debug, Deserialize)]
pub struct AcoustIdResult {
    pub id: String, // AcoustID track ID
    pub score: f32,
    #[serde(default)]
    pub recordings: Vec<AcoustIdRecording>,
}

#[derive(Debug, Deserialize)]
pub struct AcoustIdRecording {
    pub id: String, // MusicBrainz Recording ID
    #[serde(default)]
    pub duration: Option<f64>,
}
