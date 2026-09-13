//! Small shared helpers.

/// Truncate to `max` characters on a char boundary, marking the cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n... [truncated, {} chars total]", s.chars().count())
}

/// How much of a long tool answer is kept from each end, when the whole of it goes to a
/// file instead.
///
/// The head is where a command says what it is doing, and the tail is where a build log
/// says what went wrong -- which is the part that is actually being looked for. Cutting
/// the middle out and keeping both ends is the difference between "the answer is the last
/// line, which is gone" and "the answer is the last line".
pub const KEEP_HEAD: usize = 4096;
pub const KEEP_TAIL: usize = 1024;

/// Keep both ends of `text`, dropping the middle, and say where the whole of it is.
///
/// `note` is what to tell the reader about the rest -- the path it was written to, or why
/// it could not be written. Never a path that does not exist: a truncation message that
/// points at nothing is worse than one that admits the loss.
pub fn head_and_tail(text: &str, max: usize, note: &str) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    let head = KEEP_HEAD.min(max / 2);
    let tail = KEEP_TAIL.min(max / 4);
    let head_text: String = text.chars().take(head).collect();
    let tail_text: String = text.chars().skip(total - tail).collect();
    format!(
        "{head_text}\n... [{total} characters; kept the first {head} and the last {tail}; {note}]\n{tail_text}"
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Both ends are kept, the middle is what goes, and the note is where the reader is
    /// told to look. Cutting the tail -- the old behaviour -- loses the last line of a
    /// build log, which is the line being looked for.
    #[test]
    fn a_long_answer_keeps_its_first_and_last_lines() {
        let text: String = (0..2000).map(|n| format!("line {n}\n")).collect();
        let out = head_and_tail(&text, 5000, "full output: /tmp/1.txt");
        assert!(out.starts_with("line 0\n"), "the head is missing");
        assert!(out.ends_with("line 1999\n"), "the tail is missing");
        assert!(out.contains("full output: /tmp/1.txt"), "no note: {out}");
        assert!(out.contains("kept the first"), "no counts: {out}");
        assert!(
            !out.contains("line 1000\n"),
            "the middle was kept, so this is not a cut at all"
        );
        assert!(out.chars().count() < text.chars().count(), "nothing was cut");
    }

    /// A short answer is not touched, and in particular gets no note: a marker on output
    /// that was not truncated reads as if something had been lost.
    #[test]
    fn a_short_answer_is_left_alone() {
        let out = head_and_tail("hello", 5000, "full output: /tmp/1.txt");
        assert_eq!(out, "hello");
    }

    /// The cut is by character, so a multi-byte character cannot be split in half.
    #[test]
    fn a_cut_never_lands_inside_a_character() {
        let text = "字".repeat(3000);
        let out = head_and_tail(&text, 4000, "full output: /tmp/1.txt");
        assert!(out.contains('字'));
        assert!(!out.contains('\u{fffd}'), "a character was split: {out}");
    }
}
