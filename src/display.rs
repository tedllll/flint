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

/// How much of the agent's own activity to narrate, by name.
///
/// The named form of the three levels, because the level is what the printer compares and the
/// *word* is what everything else speaks: `/verbose off|on|full` takes it, `config.toml` stores
/// it, and the `state` frame a page draws its switch from carries it. All three of those used to
/// be worked out separately, and the config's copy was a `bool` -- where `false` meant "on" as
/// well as "off", so the quietest setting could be chosen and never kept.
///
/// It lives beside the levels so that the two directions cannot drift: a word that sets one level
/// and a level that prints as another is a switch that labels itself one way and sets the other,
/// which is the shape of bug the `state` frame exists to end.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Verbosity {
    /// Only the model's words.
    Off,
    /// One line per tool call. The default, and what an old `verbose = false` means.
    #[default]
    On,
    /// Tool arguments, the reasoning marker, and more of each result.
    Full,
}

impl Verbosity {
    /// All of them, in order, for anything that offers the choice -- `/verbose`'s help text and
    /// the page's switch both need the same list, and a list written twice is a list that drifts.
    pub const ALL: [Verbosity; 3] = [Verbosity::Off, Verbosity::On, Verbosity::Full];

    /// The level the printer compares against.
    pub fn level(self) -> u8 {
        match self {
            Verbosity::Off => QUIET,
            Verbosity::On => NORMAL,
            Verbosity::Full => CHATTY,
        }
    }

    /// The name for a level. Anything above `NORMAL` is `Full`, so a level added later does not
    /// silently become the default here.
    pub fn from_level(level: u8) -> Self {
        match level {
            QUIET => Verbosity::Off,
            CHATTY.. => Verbosity::Full,
            _ => Verbosity::On,
        }
    }

    /// What this is called -- in the command, in the config file, and on the page.
    pub fn word(self) -> &'static str {
        match self {
            Verbosity::Off => "off",
            Verbosity::On => "on",
            Verbosity::Full => "full",
        }
    }

    /// The setting a word names, or `None` for a word that names nothing.
    ///
    /// The long forms are accepted because `/verbose` has always taken them (`quiet`, `normal`,
    /// `all`), and a word that worked yesterday must not stop working because a page grew a
    /// picker.
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "off" | "quiet" => Some(Verbosity::Off),
            "on" | "normal" => Some(Verbosity::On),
            "full" | "all" => Some(Verbosity::Full),
            _ => None,
        }
    }
}

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
        let what = self.subject(name, args);
        let head = self.style(CYAN, "\u{23f5}");
        let label = self.style(BOLD, &verb(name));
        if what.is_empty() {
            self.emit(format_args!("{head} {label}"));
        } else {
            self.emit(format_args!("{head} {label} {}", self.dim(&what)));
        }
    }

    /// What a tool was aimed at, with paths shortened against the session directory.
    ///
    /// One place, because the call line and the result line describe the same call: when
    /// only one of them shortened the path they disagreed, and the result line -- the one
    /// that stays on screen longest -- was the one that kept the full prefix.
    fn subject(&self, name: &str, args: &str) -> String {
        let term = self.term;
        summarise_args_in(name, args, ARG_LIMIT, Some(&|p: &str| term.shorten_path(p)))
    }

    /// One line describing what a tool call produced.
    ///
    /// Never the output itself. A directory listing, a file, or a command's stdout
    /// can be hundreds of lines, and printing them buries the conversation under
    /// material the model has already read -- the reader wants to know *that*
    /// something happened, and only needs the detail when something went wrong.
    ///
    /// So: a glyph, a verb, *what it was done to*, and a summary measured in lines. The
    /// subject matters more than it looks: `✓ read 121 lines` twice in a row is
    /// unreadable, because nothing distinguishes two calls from one call printed twice --
    /// which is exactly the fault this transcript had. A failure keeps its first line,
    /// because that is the one thing a reader may have to act on, and `/detail` can still
    /// be turned on for the rest.
    pub fn tool_result(&self, name: &str, args: &str, output: &str, ok: bool) {
        if self.verbosity() == QUIET {
            return;
        }
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        let subject = self.subject(name, args);
        let label = if subject.is_empty() {
            verb(name)
        } else {
            format!("{} {}", verb(name), self.dim(&subject))
        };
        let mark = if ok {
            self.style(GREEN, "\u{2713}")
        } else {
            self.style(RED, "\u{2717}")
        };

        let summary = match lines.len() {
            0 => "no output".to_string(),
            // The first line is usually a headline the tool wrote itself
            // ("✓ list C:\Users\you", "12 lines"), so it is worth the space.
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
    summarise_args_in(name, args, limit, None)
}

/// `summarise_args`, with the session directory available for shortening paths.
///
/// Split rather than changing the signature everywhere: most callers -- tests, and the
/// reasoning about what a tool was aimed at -- do not care about the prefix, and only
/// the printer has a terminal to shorten against.
pub fn summarise_args_in(
    name: &str,
    args: &str,
    limit: usize,
    shorten: Option<&dyn Fn(&str) -> String>,
) -> String {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return util::preview(trimmed, limit);
    };
    let str_at = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);

    // The field that says *what the call was aimed at*, per tool.
    //
    // Getting this wrong is worse than showing nothing. `glob` and `grep` used to fall
    // through to "the first string argument", and for `grep` that is `path` -- so the
    // transcript read `grep *.rs` for a call whose whole point was to search for
    // `MAX_ATTEMPTS`. The one line the user gets about the call named the wrong thing.
    let picked = match name {
        "bash" => str_at("command"),
        // A program and its arguments, as the line a person would have typed. Showing
        // `program` alone -- which the fallback would do -- names `git` for a call whose
        // point is which git verb, and says nothing about the files involved.
        "exec" => str_at("program").map(|program| {
            let mut line = program;
            if let Some(items) = v.get("args").and_then(|a| a.as_array()) {
                for item in items {
                    if let Some(text) = item.as_str() {
                        line.push(' ');
                        line.push_str(text);
                    }
                }
            }
            line
        }),
        "read" | "write" | "edit" | "list" => str_at("path"),
        "glob" => str_at("pattern"),
        // A patch is its file list. The text itself is dozens of lines, and the one thing
        // worth a line in the transcript is what the call is about to touch -- the
        // fallback would print `*** Begin Patch`, which says nothing at all.
        "apply_patch" => str_at("patch").map(|text| match crate::patch::parse(&text) {
            Ok(changes) => changes
                .iter()
                .map(|change| {
                    let path = match shorten {
                        Some(shorten) => shorten(change.path()),
                        None => change.path().to_string(),
                    };
                    format!("{} {path}", change.verb())
                })
                .collect::<Vec<_>>()
                .join(", "),
            // A patch that does not parse is about to fail with the reason; showing its
            // first line here is more use than showing nothing.
            Err(_) => util::preview(&text, 60),
        }),
        // Both halves matter for a search: what was looked for, and where.
        "grep" => str_at("pattern").map(|needle| match str_at("glob") {
            Some(filter) => format!("{needle}  in {filter}"),
            None => needle,
        }),
        // Unknown tool: show the first string it was given rather than nothing.
        _ => v
            .as_object()
            .and_then(|o| o.values().find_map(|val| val.as_str()).map(str::to_string)),
    };
    let Some(picked) = picked else {
        return util::preview(trimmed, limit);
    };

    // A path is only shortened for display; the model still gets the full one it wrote. Shortened
    // *before* the range below is appended, because the other order had to find the path again by
    // splitting the line at its first space -- and a path with a space in it has more than one, so
    // `read "C:\Program Files\x.md" lines 1-20` was shortened to `…\Program Files\x.md`.
    let picked = match (shorten, name) {
        (Some(shorten), "read" | "write" | "edit" | "list") => shorten(&picked),
        _ => picked,
    };
    // A path with a space in it goes in quotes, which is the only thing in the line that says where
    // the name ends -- to the person reading the transcript *and* to the page, whose own scanner
    // reads a quoted run as one path and cuts a bare one at the space (`addressParts` in
    // `web/view.html`). One line printed here is what both of them read.
    let picked = match name {
        "read" | "write" | "edit" | "list" => quote_if_spaced(&picked),
        _ => picked,
    };

    // A paged read is only comprehensible with its range: `read src/agent.rs` twice in a
    // row is exactly the unreadable repetition the tool-result line was fixed for. The range stays
    // *outside* the quotes, so the line still says which part of a file whose name has a space.
    let summary = if name == "read" {
        match (v.get("offset").and_then(serde_json::Value::as_u64), v.get("limit").and_then(serde_json::Value::as_u64)) {
            (Some(offset), Some(limit)) if limit > 0 => {
                format!("{picked} lines {offset}-{}", offset + limit - 1)
            }
            (Some(offset), _) => format!("{picked} from line {offset}"),
            _ => picked,
        }
    } else {
        picked
    };
    util::preview(&summary.replace('\n', " \u{23ce} "), limit)
}

/// A path with a space in it, in double quotes. Everything else is returned unchanged.
///
/// Not decoration. `read C:\Program Files\flint\config.toml` is a path and a word, and which is
/// which cannot be recovered from the line: two readers of it exist -- a person, and the browser
/// page that turns a path in the transcript into a button onto the file (`GET /file`) -- and the
/// page's scanner cut that line at the space, so it drew a link to `C:\Program`, a name that exists
/// nowhere. The quotes are the one thing in the line that says where the name ends, which is why
/// they are added here, where the line is written, rather than guessed at by each reader.
///
/// Only for a space: a path of ordinary characters is unambiguous already, and quoting every path
/// would put punctuation in every tool line to no purpose.
fn quote_if_spaced(path: &str) -> String {
    if path.contains(' ') {
        format!("\"{path}\"")
    } else {
        path.to_string()
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
    fn a_path_with_a_space_in_it_is_quoted() {
        // A bare path with a space is two things to every reader of the line, and the second reader
        // is a machine: the page's address scanner cuts a bare path at the space, so an unquoted
        // `C:\Program Files\x.md` became a button onto `C:\Program`, a file that exists nowhere.
        let args = r#"{"path":"C:\\Program Files\\flint\\config.toml"}"#;
        assert_eq!(
            summarise_args("read", args, 200),
            "\"C:\\Program Files\\flint\\config.toml\"",
            "the quotes are what says where the name ends"
        );
        // A path with no space in it is left exactly as it was: the quotes are for the ambiguous
        // case, and putting them on every path would be punctuation for nothing.
        assert_eq!(summarise_args("read", r#"{"path":"src/lib.rs"}"#, 100), "src/lib.rs");
        // The range of a paged read stays *outside* the quotes, so the line still reads as one
        // statement about one file.
        assert_eq!(
            summarise_args("read", r#"{"path":"C:\\a b\\x.md","offset":100,"limit":50}"#, 200),
            "\"C:\\a b\\x.md\" lines 100-149"
        );
    }

    #[test]
    fn shortening_happens_before_the_range_is_appended() {
        // The order is the bug this guards: shortening used to run *after* the range was appended,
        // and found the path by splitting the line at its first space -- so for a file named
        // `x.md` in `C:\a b\`, the shortener was handed `C:\a` and the rest of the line went with
        // it. The shortener here marks what it was given, so both facts are visible in one line:
        // it received the whole path, and the quotes went on *after* it had run.
        let shorten = |p: &str| format!("<{p}>");
        let got = summarise_args_in("read", r#"{"path":"C:\\a b\\x.md","offset":100,"limit":50}"#, 200, Some(&shorten));
        assert_eq!(got, "\"<C:\\a b\\x.md>\" lines 100-149", "the shortener's input was the whole path: {got}");
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

    /// A patch shows the files it touches, with what it does to each.
    ///
    /// The fallback would print the patch's first line, `*** Begin Patch`, which is a line
    /// about nothing: the patch text is dozens of lines and the only part worth showing is
    /// what it is about to change.
    #[test]
    fn a_patch_shows_the_files_it_touches() {
        let args = r#"{"patch":"*** Begin Patch\n*** Add File: new.txt\n+hello\n*** Update File: old.txt\n-before\n+after\n*** Delete File: dead.txt\n*** End Patch\n"}"#;
        assert_eq!(
            summarise_args("apply_patch", args, 100),
            "add new.txt, update old.txt, delete dead.txt"
        );
    }

    /// A patch that does not parse still says something readable, because the call is
    /// about to fail and the transcript should not read as if it had been empty.
    #[test]
    fn a_malformed_patch_still_shows_something() {
        let out = summarise_args("apply_patch", r#"{"patch":"not a patch at all"}"#, 100);
        assert!(!out.is_empty(), "a malformed patch showed nothing");
        assert!(out.contains("not a patch"), "{out}");
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

    /// An `exec` call reads as the line a person would have typed.
    ///
    /// The fallback would show the first string argument, which is `program` -- so a
    /// `git commit` would appear in the transcript as `git`, twice in a row with different
    /// arguments looking identical, and with nothing about the files it touches. The
    /// arguments have to be in the line, and a `--long=value` pair kept whole.
    #[test]
    fn exec_shows_the_program_and_its_arguments() {
        let args = r#"{"program":"git","args":["commit","-m","fix: something","--author=a b"]}"#;
        assert_eq!(
            summarise_args("exec", args, 100),
            "git commit -m fix: something --author=a b"
        );
    }

    /// A program with no arguments is just its name, and an argument holding a newline
    /// cannot be allowed to break the one-line transcript.
    #[test]
    fn exec_survives_an_empty_list_and_a_multi_line_argument() {
        assert_eq!(summarise_args("exec", r#"{"program":"date"}"#, 100), "date");
        let out = summarise_args("exec", r#"{"program":"git","args":["commit","-m","a\nb"]}"#, 100);
        assert_eq!(out.lines().count(), 1, "the summary must stay one line: {out:?}");
        assert!(out.contains("a") && out.contains("b"), "{out}");
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
        let mut s = String::from("\u{2713} list C:\\Users\\you\n");
        for i in 0..45 {
            s.push_str(&format!("  entry-{i}/\n"));
        }
        s
    }

    /// Everything a printer emits for one tool result, colour off so the assertions
    /// read as plain text.
    fn result_lines(verbosity: u8, detail: bool, name: &str, output: &str, ok: bool) -> Vec<String> {
        result_lines_with_args(verbosity, detail, name, "", output, ok)
    }

    fn result_lines_with_args(
        verbosity: u8,
        detail: bool,
        name: &str,
        args: &str,
        output: &str,
        ok: bool,
    ) -> Vec<String> {
        let term = Term::plain();
        let printer = Printer::new(false, verbosity, &term);
        printer.set_tool_detail(detail);
        printer.tool_result(name, args, output, ok);
        printer.take_recorded()
    }

    #[test]
    fn a_tool_result_names_what_it_was_done_to() {
        // Two reads of two files must not read as the same line twice, which is how a
        // real duplicate in the transcript went unnoticed for a while.
        let one = result_lines_with_args(NORMAL, false, "read", r#"{"path":"src/lib.rs"}"#, "14 lines", true);
        let two = result_lines_with_args(NORMAL, false, "read", r#"{"path":"Cargo.toml"}"#, "55 lines", true);
        assert!(one[0].contains("src/lib.rs"), "no path: {one:?}");
        assert!(two[0].contains("Cargo.toml"), "no path: {two:?}");
        assert_ne!(one[0], two[0], "two different reads printed identically");
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
        printer.tool_result("bash", "ls -la", &a_big_listing(), true);
        assert!(
            printer.take_recorded().is_empty(),
            "quiet printer printed something"
        );
    }
}

#[cfg(test)]
mod summarise_new_tools {
    use super::summarise_args;

    #[test]
    fn glob_shows_its_pattern_not_a_path() {
        let args = r#"{"pattern":"**/*.rs","path":"src"}"#;
        assert_eq!(summarise_args("glob", args, 100), "**/*.rs");
    }

    #[test]
    fn grep_shows_the_needle_and_the_filter() {
        // The reported fault: this rendered as `grep *.rs`, naming the filter and hiding
        // the search term -- so the one line about the call said the wrong thing, and
        // every grep looked like it searched for a glob.
        let args = r#"{"pattern":"MAX_ATTEMPTS","glob":"*.rs"}"#;
        assert_eq!(summarise_args("grep", args, 100), "MAX_ATTEMPTS  in *.rs");
    }

    #[test]
    fn grep_without_a_filter_shows_just_the_needle() {
        let args = r#"{"pattern":"needle","path":"src"}"#;
        assert_eq!(summarise_args("grep", args, 100), "needle");
    }

    #[test]
    fn a_paged_read_shows_its_range() {
        // `read src/agent.rs` twice in a row says nothing about what differs; the range
        // is the only thing that makes the second call readable.
        let args = r#"{"path":"src/agent.rs","offset":100,"limit":50}"#;
        assert_eq!(summarise_args("read", args, 100), "src/agent.rs lines 100-149");
        let args = r#"{"path":"src/agent.rs","offset":100}"#;
        assert_eq!(summarise_args("read", args, 100), "src/agent.rs from line 100");
        // An unpaged read keeps its plain form.
        let args = r#"{"path":"src/agent.rs"}"#;
        assert_eq!(summarise_args("read", args, 100), "src/agent.rs");
    }

    #[test]
    fn an_unknown_tool_still_shows_something() {
        let args = r#"{"whatever":"value"}"#;
        assert_eq!(summarise_args("mystery", args, 100), "value");
    }
}
