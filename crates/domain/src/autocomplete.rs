#[derive(Debug, PartialEq, Eq)]
pub enum AutocompleteMode<'a> {
    /// Plain text search — query local + YouTube via heuristic
    Standard(&'a str),
    /// "yt:" prefix — bypass local, force YouTube-only results
    YoutubeOnly(&'a str),
    /// Recognised YouTube URL — show resolved preview
    YoutubeUrl { video_id: String },
    /// Non-YouTube URL — silently return empty; error on submit
    UnsupportedUrl,
}

#[derive(Debug, Clone)]
pub enum SubmissionValue {
    TrackId(uuid::Uuid),
    YoutubeVideoId(String),
    YoutubeSearch(String),
}

impl SubmissionValue {
    #[must_use]
    pub fn serialize(&self) -> String {
        match self {
            SubmissionValue::TrackId(id) => format!("tid:{id}"),
            SubmissionValue::YoutubeVideoId(vid) => format!("vid:{vid}"),
            SubmissionValue::YoutubeSearch(q) => format!("yts:{q}"),
        }
    }

    pub fn deserialize(s: &str) -> std::result::Result<Self, &'static str> {
        if let Some(rest) = s.strip_prefix("tid:") {
            if let Ok(id) = uuid::Uuid::parse_str(rest) {
                return Ok(SubmissionValue::TrackId(id));
            }
        } else if let Some(rest) = s.strip_prefix("vid:") {
            return Ok(SubmissionValue::YoutubeVideoId(rest.to_string()));
        } else if let Some(rest) = s.strip_prefix("yts:") {
            if !rest.is_empty() {
                return Ok(SubmissionValue::YoutubeSearch(rest.to_string()));
            }
        } else if let Ok(id) = uuid::Uuid::parse_str(s) {
            // backward compatibility
            return Ok(SubmissionValue::TrackId(id));
        }
        Err("invalid submission format")
    }

    /// Central routing classifier for incoming user queries
    #[must_use]
    pub fn classify(query: &str) -> Option<Self> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Ok(value) = Self::deserialize(trimmed) {
            return Some(value);
        }

        None
    }
}
