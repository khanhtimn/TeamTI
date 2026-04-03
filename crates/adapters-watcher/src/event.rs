use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FileEvent {
    /// Absolute path.
    pub path: PathBuf,
    pub kind: FileEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEventKind {
    /// File was created or modified.
    CreateOrModify,
    /// File was removed.
    Remove,
}
