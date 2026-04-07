/// Format milliseconds as `M:SS`. Returns `"--:--"` for ≤0.
#[must_use]
pub fn format_duration_ms(ms: i64) -> String {
    if ms <= 0 {
        return "--:--".to_string();
    }
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}

/// Format an `Option<i64>` duration (from DB) as `M:SS`.
/// Returns `"--:--"` for `None` or `≤0`.
#[must_use]
pub fn format_duration_opt(ms: Option<i64>) -> String {
    match ms {
        Some(v) if v > 0 => format_duration_ms(v),
        _ => "--:--".to_string(),
    }
}

/// Unicode progress bar.
///
/// - `filled`  = `━`
/// - `cursor`  = `●` (playing) or `⏸` (paused)
/// - `empty`   = `─`
///
/// Returns a line of `─` if `total_ms` is unknown/zero.
#[must_use]
pub fn progress_bar(elapsed_ms: i64, total_ms: i64, width: usize, paused: bool) -> String {
    if total_ms <= 0 || width == 0 {
        return "─".repeat(width);
    }
    let ratio = (elapsed_ms as f64 / total_ms as f64).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width.saturating_sub(1));
    let cursor = if paused { '⏸' } else { '●' };
    format!(
        "{}{}{}",
        "━".repeat(filled),
        cursor,
        "─".repeat(width.saturating_sub(filled + 1))
    )
}
