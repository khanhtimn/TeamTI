/// Reference to a file stored relative to MEDIA_ROOT.
#[derive(Debug, Clone)]
pub struct ManagedBlobRef {
    /// Path relative to MEDIA_ROOT, e.g. "Artist/Album/track.flac"
    pub relative_path: String,
}

#[derive(Debug, Clone)]
pub enum PlayableSource {
    ResolvedPlayable {
        path: String,
        duration_ms: Option<i64>,
    },
    UnresolvedRemote(String),
}
