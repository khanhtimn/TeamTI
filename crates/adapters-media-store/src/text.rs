use unicode_normalization::{UnicodeNormalization, is_nfc_quick, IsNormalized};

/// Normalize a string to NFC form.
/// Uses a quick pre-check to avoid allocation for already-normalized strings.
/// This is a no-op for ASCII-only strings (is_nfc_quick returns Yes).
pub(crate) fn normalize(s: &str) -> String {
    match is_nfc_quick(s.chars()) {
        IsNormalized::Yes => s.to_owned(),
        _ => s.nfc().collect(),
    }
}

/// Normalize an optional string. None passes through unchanged.
pub(crate) fn normalize_opt(s: Option<String>) -> Option<String> {
    s.map(|v| normalize(&v))
}

/// Extract the filename stem from a path and normalize it to NFC.
/// Falls back to empty string if the path has no filename.
#[allow(dead_code)]
pub(crate) fn normalize_filename_stem(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    normalize(stem)
}

/// Extract the full filename (with extension) from a path, normalized to NFC.
pub(crate) fn normalize_filename(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    normalize(name)
}
