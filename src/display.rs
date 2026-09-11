//! Terminal output.
//!
//! Everything the user reads while flint is working goes through here, so that
//! there is exactly one place to decide what is worth printing. The default is
//! deliberately quiet: a rescue tool is used when something has already gone
//! wrong, and a wall of tool output makes that worse rather than better.

use crate::term::Term;
use crate::util;

pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const CYAN: &str = "\x1b[36m";
pub const YELLOW: &str = "\x1b[33m";
pub const RESET: &str = "\x1b[0m";

/// The colour codes to interpolate into a format string, or empty strings.
///
/// `{BOLD}` written directly into a `format_args!` always emits the escape code,
/// whatever `color` says -- which is how a redirected `flint` ends up with ANSI
/// noise in a pipeline. Passing a palette instead makes the choice explicit at
/// every call site that has one.
///
/// The aliases are deliberately spelled like the constants they replace: a
/// function can shadow `BOLD` with `p.bold` by destructuring, and then every
/// existing format string follows the colour setting with no edit at all.
#[derive(Clone, Copy)]
#[allow(non_snake_case)]
pub struct Palette {
    pub dim: &'static str,
    pub bold: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    pub cyan: &'static str,
    pub yellow: &'static str,
    pub reset: &'static str,
}

pub const PLAIN: Palette = Palette {
    dim: "",
    bold: "",
    red: "",
    green: "",
    cyan: "",
    yellow: "",
    reset: "",
};

pub const COLOR: Palette = Palette {
    dim: DIM,
    bold: BOLD,
    red: RED,
    green: GREEN,
    cyan: CYAN,
    yellow: YELLOW,
    reset: RESET,
};

impl Palette {
    pub fn of(color: bool) -> Palette {
        if color {
            COLOR
        } else {
            PLAIN
        }
    }
}

/// Verbosity levels, in the order you would turn them up.
pub const QUIET: u8 = 0;
pub const NORMAL: u8 = 1;
pub const CHATTY: u8 = 2;

/// Result-line budget, so a runaway tool cannot flood the terminal.
const COMPACT_GIST: usize = 120;
const CHATTY_GIST_LINES: usize = 25;
const CHATTY_ARG: usize = 400;
const COMPACT_ARG: usize = 100;

pub struct Printer<'a> {
    pub color: bool,
    /// QUIET shows only the model's words; NORMAL adds one line per tool call;
    /// CHATTY adds full arguments and more of each result.
    ///
    /// Interior mutability because `/verbose` changes it mid-session while the
    /// printer is shared as `&Printer`.
    verbosity: std::cell::Cell<u8>,
    /// Colour codes matching this printer's colour setting.
    ///
    /// Interpolating `{p.bold}` collapses to nothing when colour is off, whereas
    /// the bare `BOLD` constant never does. Prefer this in any text that may be
    /// piped.
    pub pal: Palette,
    /// Where output goes, so that every line lands in the terminal's scroll region
    /// rather than fighting the input row for the same cursor.
    term: &'a Term,
}

impl<'a> Printer<'a> {
    pub fn new(color: bool, verbosity: u8, term: &'a Term) -> Self {
        Printer {
            color,
            verbosity: std::cell::Cell::new(verbosity),
            pal: Palette::of(color),
            term,
        }
    }

    pub fn verbosity(&self) -> u8 {
        self.verbosity.get()
    }

    pub fn set_verbosity(&self, level: u8) {
        self.verbosity.set(level);
    }

    /// The terminal this printer writes to. Used by call sites that need to emit
    /// a line the printer has no opinion about.
    pub fn term(&self) -> &'a Term {
        self.term
    }

    pub fn style(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.style(DIM, text)
    }

    /// One compact line describing a tool call that is about to run.
    ///
    /// The argument is reduced to the one thing worth reading -- the command for
    /// `bash`, the path for a file tool -- because a raw JSON blob tells the
    /// reader nothing the result will not tell them better.
    pub fn tool_call(&self, name: &str, args: &str) {
        if self.verbosity() == QUIET {
            return;
        }
        let limit = if self.verbosity() >= CHATTY {
            CHATTY_ARG
        } else {
            COMPACT_ARG
        };
        let what = summarise_args(name, args, limit);
        let head = self.style(CYAN, "\u{23f5}");
        let label = self.style(BOLD, name);
        if what.is_empty() {
            self.term.line(format_args!("{head} {label}"));
        } else {
            self.term.line(format_args!("{head} {label} {}", self.dim(&what)));
        }
    }

    /// The result of a tool call: a status glyph, a one-line gist, and at most a
    /// couple of lines of detail.
    pub fn tool_result(&self, output: &str, ok: bool) {
        if self.verbosity() == QUIET {
            return;
        }
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        let gist = lines.first().copied().unwrap_or("(no output)");
        let mark = if ok {
            self.style(GREEN, "\u{2713}")
        } else {
            self.style(RED, "\u{2717}")
        };

        if self.verbosity() >= CHATTY {
            self.term.line(format_args!("  {mark} {}", self.dim(gist)));
            for line in lines.iter().skip(1).take(CHATTY_GIST_LINES - 1) {
                self.term.line(format_args!("    {}", self.dim(line)));
            }
            if lines.len() > CHATTY_GIST_LINES {
                self.term.line(format_args!(
                    "    {}",
                    self.dim(&format!("… {} more lines", lines.len() - CHATTY_GIST_LINES))
                ));
            }
            return;
        }

        // Compact: one line, plus a count of what is being deliberately withheld
        // so the reader knows the agent saw more than they did.
        let extra = if lines.len() > 1 {
            format!("  {}", self.dim(&format!("(+{} lines)", lines.len() - 1)))
        } else {
            String::new()
        };
        let gist: String = gist.chars().take(COMPACT_GIST).collect();
        let ellipsis = if gist.chars().count() < lines.first().map_or(0, |l| l.chars().count()) {
            "…"
        } else {
            ""
        };
        self.term.line(format_args!("  {mark} {}{ellipsis}{extra}", self.dim(&gist)));
    }

    /// A turn was stopped because the user typed something.
    pub fn interrupted(&self) {
        self.term.line(format_args!("{}", self.style(YELLOW, "\u{23f9} interrupted")));
    }
}

/// Reduce a tool's JSON arguments to the one value worth showing on a line.
pub fn summarise_args(name: &str, args: &str, limit: usize) -> String {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return util::preview(trimmed, limit);
    };
    let key = match name {
        "bash" => "command",
        "read" | "write" | "edit" | "list" => "path",
        _ => "",
    };
    let picked = v
        .get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        // Unknown tool: show the first string it was given rather than nothing.
        .or_else(|| {
            v.as_object()
                .and_then(|o| o.values().find_map(|val| val.as_str()).map(str::to_string))
        });
    match picked {
        Some(s) => util::preview(&s.replace('\n', " \u{23ce} "), limit),
        None => util::preview(trimmed, limit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_shows_the_command_and_nothing_else() {
        let args = r#"{"command":"dir C:\\tmp","timeout_secs":30}"#;
        assert_eq!(summarise_args("bash", args, 100), "dir C:\\tmp");
    }

    #[test]
    fn file_tools_show_the_path() {
        let args = r#"{"path":"C:\\Users\\me\\notes.md","content":"hello"}"#;
        for tool in ["read", "write", "edit", "list"] {
            assert_eq!(
                summarise_args(tool, args, 100),
                "C:\\Users\\me\\notes.md",
                "{tool} should show the path"
            );
        }
    }

    #[test]
    fn an_unknown_tool_still_shows_something_useful() {
        // Nothing here should be dropped on the floor, but a reader cannot do
        // anything with an empty line either.
        assert_eq!(
            summarise_args("mystery", r#"{"whatever":"visible"}"#, 100),
            "visible"
        );
    }

    #[test]
    fn empty_arguments_produce_no_suffix() {
        assert_eq!(summarise_args("bash", "{}", 100), "");
        assert_eq!(summarise_args("bash", "   ", 100), "");
    }

    #[test]
    fn malformed_arguments_are_shown_verbatim_rather_than_swallowed() {
        // A model emitting broken JSON is worth seeing, not hiding.
        let out = summarise_args("bash", "{not json", 100);
        assert!(out.contains("not json"), "got {out:?}");
    }

    #[test]
    fn long_arguments_are_clipped() {
        let args = format!(r#"{{"command":"{}"}}"#, "x".repeat(500));
        let out = summarise_args("bash", &args, 50);
        assert!(
            out.chars().count() < 80,
            "not clipped: {} chars",
            out.chars().count()
        );
    }

    #[test]
    fn newlines_do_not_break_the_single_line_layout() {
        let args = r#"{"command":"line one\nline two"}"#;
        let out = summarise_args("bash", args, 100);
        assert!(!out.contains('\n'), "command kept a newline: {out:?}");
    }
}
