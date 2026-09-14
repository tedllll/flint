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

/// Decode one piece of a child's output.
///
/// `from_utf8_lossy` is exact for anything that writes UTF-8 and wrong for a Windows console
/// program, which writes the console's code page instead. On a machine with a Chinese locale
/// that is 936, so every non-ASCII character became U+FFFD: the model was shown a system whose
/// error messages it could not read, and told nothing about why. It then reasons about a
/// machine that answers `??????`.
///
/// Trying UTF-8 first keeps the common case byte-exact -- a program that writes UTF-8 is never
/// re-interpreted -- and the fallback is only reached by bytes that failed it. See
/// [`decode_code_page`] for why the conversion is declared here rather than bought.
/// Try UTF-8 first, then the machine's own code page. See [`decode_code_page`].
pub fn decode_child_text(raw: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(raw) {
        return text.to_string();
    }
    #[cfg(windows)]
    if let Some(text) = decode_code_page(raw, ansi_code_page()) {
        return text;
    }
    String::from_utf8_lossy(raw).to_string()
}

/// The code page of the machine's locale: the one a program falls back to when it has no
/// console, and the one to decode a child's output with.
///
/// The *console* output code page would be the more obvious choice, and it is the one `cmd`,
/// `dir` and PowerShell were measured using. It is also mutable, console-wide state: `chcp`
/// changes it for every process attached to that console, and while measuring this the same
/// terminal was seen at 936 and at 65001 depending on what had run in it earlier. A key that
/// another program can change is not a key. It also cannot disagree in a way that matters:
/// when the console page *is* UTF-8, the bytes are UTF-8 and the attempt above already
/// returned; when a child writes the locale's page instead -- measured, that is what Python
/// does, ignoring `chcp` entirely -- this is the page it wrote.
#[cfg(windows)]
pub fn ansi_code_page() -> u32 {
    use windows::GetACP;
    // Safe: an argument-free query of system state.
    unsafe { GetACP() }
}

/// The three kernel32 calls this needs, declared rather than bought.
///
/// `windows-sys` was rejected for the job-object kill in `docs/windows-tooling.md` §6.1
/// because it is a crate to reach a handful of functions; the same argument applies here, and
/// the repository's rule is about dependencies, not about `unsafe`. Declaring them also means
/// there is no code-page table to maintain: the OS owns the conversion, and a hand-written
/// GBK table is exactly the kind of thing that is subtly wrong for a character nobody tested.
#[cfg(windows)]
mod windows {
    use std::os::raw::c_int;

    extern "system" {
        pub fn GetACP() -> u32;
        pub fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            bytes: *const u8,
            byte_len: c_int,
            out: *mut u16,
            out_len: c_int,
        ) -> c_int;
    }
}

/// Decode bytes written in `code_page`, or `None` when the conversion says nothing.
///
/// `flags = 0` rather than `MB_ERR_INVALID_CHARS`: that flag is only supported for UTF-8 and
/// GB18030, and the other code pages fail outright with it. Substituting is right here anyway,
/// because the bytes have already failed UTF-8 and a byte this code page cannot express is one
/// nobody can read either way.
#[cfg(windows)]
pub fn decode_code_page(raw: &[u8], code_page: u32) -> Option<String> {
    use windows::MultiByteToWideChar;
    // 65001 is UTF-8, which the caller has already tried and failed; asking again would only
    // turn the bytes into a second round of U+FFFD.
    if raw.is_empty() || code_page == 0 || code_page == 65001 {
        return None;
    }
    let len = raw.len().min(std::os::raw::c_int::MAX as usize) as std::os::raw::c_int;
    // Safe: `raw` is a live slice of exactly `len` bytes, and a null output pointer with a
    // zero count is the documented way to ask how much room is needed.
    let needed = unsafe { MultiByteToWideChar(code_page, 0, raw.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return None;
    }
    let mut wide = vec![0u16; needed as usize];
    // Safe: the buffer is `needed` elements, which is what the call above asked for.
    let written =
        unsafe { MultiByteToWideChar(code_page, 0, raw.as_ptr(), len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return None;
    }
    wide.truncate(written as usize);
    Some(String::from_utf16_lossy(&wide))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text that is already UTF-8 is never reinterpreted -- the whole point of trying it
    /// first, and the property that keeps this from being a change to the common case.
    #[test]
    fn utf8_output_is_untouched() {
        let text = "你好 café 日本語";
        assert_eq!(decode_child_text(text.as_bytes()), text);
    }

    /// The bytes a Chinese-locale Windows console writes for `你好`: valid GBK, and not valid
    /// UTF-8 (`c4 e3` is a two-byte GBK sequence and a broken UTF-8 lead byte). Those are the
    /// bytes that used to reach the model as two replacement characters.
    #[cfg(windows)]
    #[test]
    fn a_console_code_page_piece_is_decoded_rather_than_replaced() {
        let gbk = [0xc4u8, 0xe3, 0xba, 0xc3];
        assert_eq!(
            decode_code_page(&gbk, 936).as_deref(),
            Some("你好"),
            "CP936 bytes must decode to the text they are"
        );
    }

    /// A code page that cannot express the bytes still says something rather than refusing:
    /// the alternative is an empty tool result.
    #[cfg(windows)]
    #[test]
    fn a_code_page_conversion_always_answers() {
        assert!(decode_code_page(&[0xff, 0xfe, 0x00], 936).is_some());
        assert!(decode_code_page(&[], 936).is_none(), "nothing is not text");
        assert!(
            decode_code_page(b"hello", 65001).is_none(),
            "UTF-8 is the caller's first attempt, not this function's second"
        );
    }

    /// Off Windows the old behaviour is the whole behaviour, and it is written down as a test
    /// so that a later session cannot mistake the fallback for something that exists here.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_a_non_utf8_piece_is_still_replaced() {
        let out = decode_child_text(&[0xc4, 0xe3, 0xba, 0xc3]);
        assert!(out.contains('\u{fffd}'), "got {out:?}");
    }

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
