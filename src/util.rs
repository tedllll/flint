//! Small shared helpers.

/// Truncate to `max` characters on a char boundary, marking the cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n... [truncated, {} chars total]", s.chars().count())
}

/// First line of a string, clipped. Used for one-line tool previews.
pub fn preview(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    let clipped: String = first.chars().take(max).collect();
    if s.lines().count() > 1 || first.chars().count() > max {
        format!("{clipped} ...")
    } else {
        clipped
    }
}

/// Take at most `max` characters, collapsed onto one line and marked if cut.
///
/// Unlike [`truncate`], this is for text that must stay a single line -- a catalog entry,
/// a list item -- so it flattens whitespace and never reports a count.
pub fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", head.trim_end())
}

/// Collapse a JSON string / object into a compact one-line preview.
pub fn json_preview(raw: &str, max: usize) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => preview(&v.to_string(), max),
        Err(_) => preview(raw, max),
    }
}

/// Escape control characters so tool output cannot wreck the terminal.
pub fn sanitize_output(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}
