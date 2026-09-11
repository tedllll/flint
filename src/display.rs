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

/// Budgets, so a runaway tool cannot flood the terminal.
///
/// The summary is what the reader sees; the rest is available at `/verbose`.
const GIST_LIMIT: usize = 60;
const FAIL_LIMIT: usize = 200;
const ARG_LIMIT: usize = 80;
/// How many lines one `/verbose` tool result is allowed to occupy, header included.
pub const CHATTY_GIST_LINES: usize = 25;

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
    /// Every line this printer has emitted, in order.
    ///
    /// This is what makes "a tool result is one line, and never the output itself"
    /// a testable claim. The alternative was to capture the process's stdout by
    /// redirecting a file descriptor, which cannot work reliably on Windows while
    /// the test harness is writing to the same stream. Recording at the point of
    /// emission tests the real code path without involving the process at all.
    recorded: std::cell::RefCell<Vec<String>>,
    /// Whether tool *output* is shown, as opposed to a summary of it.
    ///
    /// Separate from verbosity on purpose: verbosity is about how much of the
    /// model's own activity to narrate, and this is about whether to print the
    /// contents of a file the model just read. Defaults to off, because output is
    /// measured in hundreds of lines and the transcript is meant to stay readable.
    tool_detail: std::cell::Cell<bool>,
}

impl<'a> Printer<'a> {
    pub fn new(color: bool, verbosity: u8, term: &'a Term) -> Self {
        Printer {
            color,
            verbosity: std::cell::Cell::new(verbosity),
            pal: Palette::of(color),
            term,
            recorded: std::cell::RefCell::new(Vec::new()),
            tool_detail: std::cell::Cell::new(false),
        }
    }

    /// Emit one line, and remember it.
    fn emit(&self, args: std::fmt::Arguments<'_>) {
        self.recorded.borrow_mut().push(format!("{args}"));
        self.term.line(args);
    }

    /// Whether the output behind a tool result is shown, instead of only its size.
    ///
    /// Off by default, and deliberately not tied to verbosity: "show me more of what
    /// the model is doing" and "print the file it just read" are different wants, and
    /// conflating them means anyone who turns up the first gets the second. A
    /// directory listing is forty lines; reading a file is hundreds. They belong
    /// behind their own switch.
    pub fn set_tool_detail(&self, on: bool) {
        self.tool_detail.set(on);
    }

    pub fn tool_detail(&self) -> bool {
        self.tool_detail.get()
    }

    /// Take everything recorded so far, clearing the record.
    #[cfg(test)]
    fn take_recorded(&self) -> Vec<String> {
        std::mem::take(&mut self.recorded.borrow_mut())
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

    /// One line describing a tool call that is about to run.
    ///
    /// The argument is reduced to the one thing worth reading -- the command for
    /// `bash`, the path for a file tool -- because a raw JSON blob tells the
    /// reader nothing the result will not tell them better.
    pub fn tool_call(&self, name: &str, args: &str) {
        if self.verbosity() == QUIET {
            return;
        }
        let what = summarise_args(name, args, ARG_LIMIT);
        let head = self.style(CYAN, "\u{23f5}");
        let label = self.style(BOLD, &verb(name));
        if what.is_empty() {
            self.emit(format_args!("{head} {label}"));
        } else {
            self.emit(format_args!("{head} {label} {}", self.dim(&what)));
        }
    }

    /// One line describing what a tool call produced.
    ///
    /// Never the output itself. A directory listing, a file, or a command's stdout
    /// can be hundreds of lines, and printing them buries the conversation under
    /// material the model has already read -- the reader wants to know *that*
    /// something happened, and only needs the detail when something went wrong.
    ///
    /// So: a glyph, a verb, and a summary measured in lines. A failure keeps its
    /// first line, because that is the one thing a reader may have to act on, and
    /// `/verbose` can still be turned up for the rest.
    pub fn tool_result(&self, name: &str, output: &str, ok: bool) {
        if self.verbosity() == QUIET {
            return;
        }
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        let label = verb(name);
        let mark = if ok {
            self.style(GREEN, "\u{2713}")
        } else {
            self.style(RED, "\u{2717}")
        };

        let summary = match lines.len() {
            0 => "no output".to_string(),
            // The first line is usually a headline the tool wrote itself
            // ("✓ list C:\Users\zhangzhuo", "12 lines"), so it is worth the space.
            1 => util::preview(lines[0], GIST_LIMIT),
            n => format!("{n} lines"),
        };
        let mut line = format!("{mark} {label} {}", self.dim(&summary));

        // A failure is the exception: whatever it said, the reader probably needs
        // it, so the detail is appended rather than summarised away.
        if !ok {
            let detail = lines.first().copied().unwrap_or("(no output)");
            if lines.len() == 1 {
                line = format!("{mark} {label} {}", self.style(RED, &util::preview(detail, FAIL_LIMIT)));
            } else {
                line.push_str(&format!(" {}", self.style(RED, &util::preview(detail, FAIL_LIMIT))));
            }
        }
        self.emit(format_args!("{line}"));

        // The output itself only appears when it has been asked for, and even then
        // under a cap. `CHATTY_GIST_LINES` is the budget for the whole emission,
        // header included, so the "more lines" note is budgeted for rather than
        // added on top -- otherwise a detailed result is always one line over.
        if self.tool_detail() && lines.len() > 1 {
            let rest = lines.len() - 1;
            let budget = CHATTY_GIST_LINES - 1;
            let shown = if rest > budget { budget - 1 } else { rest };
            for line in lines.iter().skip(1).take(shown) {
                self.emit(format_args!("    {}", self.dim(line)));
            }
            if rest > shown {
                self.emit(format_args!(
                    "    {}",
                    self.dim(&format!("… {} more lines", rest - shown))
                ));
            }
        }
    }

    /// A turn was stopped because the user typed something.
    pub fn interrupted(&self) {
        self.emit(format_args!("{}", self.style(YELLOW, "\u{23f9} interrupted")));
    }
}

/// A tool name as it appears in the transcript.
///
/// The names come from the function schema and are already lowercase, so this
/// mostly guards the two things that matter: a name that arrives in another case
/// still reads consistently, and a new tool that this match has never heard of
/// still shows up instead of vanishing from the transcript.
fn verb(name: &str) -> String {
    name.trim().to_ascii_lowercase()
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

    #[test]
    fn a_tool_name_is_lowercased_for_the_transcript() {
        assert_eq!(verb("READ"), "read");
        assert_eq!(verb("Bash"), "bash");
        // A tool this match has never heard of must still appear, never vanish.
        assert_eq!(verb("some_new_tool"), "some_new_tool");
    }

    // --- what a tool result prints to the reader ---------------------------------

    /// The output of `list` on a home directory, which is what prompted this change:
    /// forty-five lines of filenames that bury the conversation.
    fn a_big_listing() -> String {
        let mut s = String::from("\u{2713} list C:\\Users\\zhangzhuo\n");
        for i in 0..45 {
            s.push_str(&format!("  entry-{i}/\n"));
        }
        s
    }

    /// Everything a printer emits for one tool result, colour off so the assertions
    /// read as plain text.
    fn result_lines(verbosity: u8, detail: bool, name: &str, output: &str, ok: bool) -> Vec<String> {
        let term = Term::plain();
        let printer = Printer::new(false, verbosity, &term);
        printer.set_tool_detail(detail);
        printer.tool_result(name, output, ok);
        printer.take_recorded()
    }

    #[test]
    fn a_tool_result_is_one_line_and_never_the_output() {
        let lines = result_lines(NORMAL, false, "list", &a_big_listing(), true);
        assert_eq!(lines.len(), 1, "expected one line, got {lines:#?}");
        assert!(lines[0].contains("list"), "no tool name: {lines:?}");
        assert!(
            !lines[0].contains("entry-"),
            "the listing leaked into the transcript: {lines:?}"
        );
    }

    #[test]
    fn detail_is_off_even_at_full_verbosity() {
        // The reported bug. `/verbose full` used to turn on tool output as well, so
        // anyone who wanted the running commentary got every directory listing and
        // file read printed into the conversation. They are different wants.
        for verbosity in [NORMAL, CHATTY] {
            let lines = result_lines(verbosity, false, "list", &a_big_listing(), true);
            assert_eq!(
                lines.len(),
                1,
                "verbosity {verbosity} printed the output anyway: {lines:#?}"
            );
        }
    }

    #[test]
    fn detail_when_asked_for_shows_some_of_it_and_still_not_everything() {
        let lines = result_lines(NORMAL, true, "list", &a_big_listing(), true);
        assert!(lines.len() > 1, "detail asked for but not shown: {lines:#?}");
        assert!(
            lines.len() <= CHATTY_GIST_LINES,
            "detail flooded the screen with {} lines",
            lines.len()
        );
        assert!(
            lines.iter().any(|l| l.contains("more lines")),
            "no note about what was withheld: {lines:#?}"
        );
    }

    #[test]
    fn a_failed_tool_result_keeps_the_reason() {
        // The exception to "never the output": if something broke, the reader may
        // have to act on it, so the reason survives even at normal verbosity.
        let lines = result_lines(NORMAL, false, "bash", "command not found: rq", false);
        assert_eq!(lines.len(), 1, "not one line: {lines:#?}");
        assert!(
            lines[0].contains("command not found"),
            "reason lost: {lines:?}"
        );
    }

    #[test]
    fn a_single_line_result_is_shown_rather_than_counted() {
        // "1 lines" would be useless when the tool already wrote a headline.
        let lines = result_lines(NORMAL, false, "read", "12 lines", true);
        assert!(lines[0].contains("12 lines"), "summary dropped: {lines:?}");
    }

    #[test]
    fn a_quiet_printer_says_nothing_about_tools() {
        let term = Term::plain();
        let printer = Printer::new(false, QUIET, &term);
        printer.tool_call("bash", r#"{"command":"dir"}"#);
        printer.tool_result("bash", &a_big_listing(), true);
        assert!(
            printer.take_recorded().is_empty(),
            "quiet printer printed something"
        );
    }
}
