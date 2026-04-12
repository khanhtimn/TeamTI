/// Sanitize a string for use as a filesystem path component.
/// Uses the `sanitize-filename` crate to handle Windows reserved names,
/// control characters, and forbidden path separators robustly.
///
/// Additional post-processing:
/// - Truncates to 100 characters (not bytes) to prevent path-too-long issues
/// - Falls back to "unknown" for empty results
pub(crate) fn sanitize_component(s: &str) -> String {
    let sanitized = sanitize_filename::sanitize_with_options(
        s,
        sanitize_filename::Options {
            // Handle Windows reserved names (CON, PRN, AUX, NUL, etc.)
            // even on Linux/macOS, since media files may be shared cross-platform
            windows: true,
            // The crate's truncation works on bytes (max 255); we also
            // apply a tighter char-level truncation below.
            truncate: true,
            replacement: "_",
        },
    );

    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }

    // Truncate to 100 chars to prevent path-too-long issues
    // (the crate's truncation is byte-level at 255, we want tighter)
    truncate_at_char_boundary(trimmed, 100).to_string()
}

fn truncate_at_char_boundary(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    &s[..s.char_indices().nth(max_chars).map_or(s.len(), |(i, _)| i)]
}

/// Extract the 11-character video ID from a YouTube URL.
#[must_use]
pub fn extract_youtube_video_id(url: &str) -> Option<String> {
    if let Some(idx) = url.find("v=") {
        let after_v = &url[idx + 2..];
        let end = after_v.find('&').unwrap_or(after_v.len());
        let id = &after_v[..end];
        if id.len() == 11 {
            return Some(id.to_string());
        }
    }
    if let Some(idx) = url.find("youtu.be/") {
        let after_slash = &url[idx + 9..];
        let end = after_slash.find('?').unwrap_or(after_slash.len());
        let id = &after_slash[..end];
        if id.len() == 11 {
            return Some(id.to_string());
        }
    }
    if let Some(idx) = url.find("youtube.com/shorts/") {
        let after_slash = &url[idx + 19..];
        let end = after_slash.find('?').unwrap_or(after_slash.len());
        let id = &after_slash[..end];
        if id.len() == 11 {
            return Some(id.to_string());
        }
    }
    None
}

/// Helper method to always build canonical watch URL from an ID.
#[must_use]
pub fn canonical_youtube_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// Extract playlist ID from URL
#[must_use]
pub fn extract_youtube_playlist_id(url: &str) -> Option<String> {
    if let Some(idx) = url.find("list=") {
        let after_list = &url[idx + 5..];
        let end = after_list.find('&').unwrap_or(after_list.len());
        return Some(after_list[..end].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_basic() {
        assert_eq!(sanitize_component("hello/world"), "hello_world");
        assert_eq!(sanitize_component("a:b*c?d"), "a_b_c_d");
    }

    #[test]
    fn test_sanitize_empty() {
        assert_eq!(sanitize_component(""), "unknown");
        let result = sanitize_component("...");
        assert!(!result.is_empty());
        assert_ne!(result, "...");
    }

    #[test]
    fn test_sanitize_preserves_unicode() {
        assert_eq!(sanitize_component("こんにちは"), "こんにちは");
    }

    #[test]
    fn test_sanitize_windows_reserved() {
        // Windows reserved names should be sanitized even on Linux
        let result = sanitize_component("CON");
        assert_ne!(result, "CON");
        assert!(!result.is_empty());
    }

    #[test]
    fn test_sanitize_truncation() {
        let long = "a".repeat(200);
        let result = sanitize_component(&long);
        assert!(result.chars().count() <= 100);
    }
}
