use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAsset {
    pub id: Uuid,
    pub title: String,
    pub origin: MediaOrigin,
    pub duration_ms: Option<u64>,
    pub content_hash: Option<String>,
    pub original_filename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaOrigin {
    LocalManaged { rel_path: String },
    Remote(String),
}

#[derive(Debug, Clone)]
pub struct ManagedBlobRef {
    pub absolute_path: String,
}

#[derive(Debug, Clone)]
pub enum PlayableSource {
    ResolvedPlayable { path: String, duration_ms: Option<u64> },
    UnresolvedRemote(String),
}
