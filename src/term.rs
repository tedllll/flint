//! The terminal, when there is one.
//!
//! Two responsibilities, both of which only apply to a real console:
//!
//! 1. A reserved input row at the bottom, using the terminal's scrolling region
//!    so that model output scrolls above it and never pushes it around. Redrawing
//!    a prompt in place is not enough: the output still scrolls the screen, and
//!    the prompt travels up with it.
//!
//! 2. Raw mode, so key presses arrive as events and the line can be edited.
//!
//! When stdout is not a terminal -- piped, redirected, run from a script -- every
//! method here degrades to a plain write. That path is not an embarrassment to be
//! tolerated: `flint exec`, CI, and `flint -p "..." | grep` all live there, and
//! they must never see an escape code.
//!
//! Everything takes `&self`. Output and input both come from one place, and the
//! callers hold this behind a shared reference, so interior mutability keeps the
//! borrow checker out of the way of what is really single-threaded use.

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Mutex;
use std::io::{IsTerminal, Write};

/// How many terminal columns `text` occupies.
///
/// Not `chars().count()`. A terminal advances two columns for a CJK ideograph or
/// other East Asian wide character, so a line of Chinese is roughly twice as wide
/// as its character count suggests. Streaming an answer has to know where the
/// cursor ended up, and the answer to that question is in columns.
///
/// The ranges are the standard East Asian Wide/Fullwidth set. Being slightly wrong
/// about an exotic code point is survivable; being wrong about ordinary Chinese is
/// not.
fn display_width(text: &str) -> u16 {
    text.chars().map(char_width).sum()
}

/// The number of *bytes* of `new` that `old` already starts with, on a character boundary.
///
/// A streamed row almost always keeps its head and grows at the end, so this is the part of
/// the row the terminal already has. It returns a byte count because it is used to slice the
/// string, and it stops on a character boundary because slicing a wide character in half is
/// not a thing that can be drawn.
fn common_prefix_len(old: &str, new: &str) -> usize {
    let mut bytes = 0;
    for (a, b) in old.chars().zip(new.chars()) {
        if a != b {
            break;
        }
        bytes += a.len_utf8();
    }
    bytes
}

fn char_width(c: char) -> u16 {
    let c = c as u32;
    // Combining marks and zero-width characters take no room of their own.
    if (0x0300..=0x036f).contains(&c) || c == 0x200b || c == 0xfeff {
        return 0;
    }
    let wide = (0x1100..=0x115f).contains(&c)        // Hangul Jamo
        || (0x2e80..=0x303e).contains(&c)            // CJK radicals and punctuation
        || (0x3041..=0x33ff).contains(&c)            // kana, CJK compatibility
        || (0x3400..=0x4dbf).contains(&c)            // CJK extension A
        || (0x4e00..=0x9fff).contains(&c)            // CJK unified ideographs
        || (0xa000..=0xa4cf).contains(&c)            // Yi
        || (0xac00..=0xd7a3).contains(&c)            // Hangul syllables
        || (0xf900..=0xfaff).contains(&c)            // CJK compatibility ideographs
        || (0xfe30..=0xfe6f).contains(&c)            // CJK compatibility forms
        || (0xff00..=0xff60).contains(&c)            // fullwidth forms
        || (0xffe0..=0xffe6).contains(&c)
        || (0x1f300..=0x1f64f).contains(&c)          // emoji
        || (0x20000..=0x3fffd).contains(&c); // CJK extensions B and beyond
    if wide {
        2
    } else {
        1
    }
}

/// Split `text` into the rows a terminal of `cols` columns would actually display.
///
/// This is not a cosmetic step. Output is committed to history one screen row at a
/// time, inside a scrolling region, and a logical line wider than the screen is
/// several screen rows. Committing it as one row loses lines and repeats others,
/// and because the repeat happens through the region's bottom margin the same
/// paragraph can smear down the whole screen.
///
/// A row breaks *before* a wide character that would not fit, so a character is
/// never split across the boundary.
fn wrap_rows(text: &str, cols: u16) -> Vec<String> {
    let cols = cols.max(1) as usize;
    let mut out = Vec::new();
    for logical in text.split('\n') {
        let mut current = String::new();
        let mut width = 0usize;
        for ch in logical.chars() {
            let cw = char_width(ch) as usize;
            if width + cw > cols && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                width = 0;
            }
            current.push(ch);
            width += cw;
        }
        out.push(current);
    }
    out
}

/// What the input row currently holds.
#[derive(Default, Clone)]
pub struct Input {
    pub buf: Vec<char>,
    pub pos: usize,
}

impl Input {
    pub fn text(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.pos, c);
        self.pos += 1;
    }

    pub fn backspace(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
            self.buf.remove(self.pos);
        }
    }

    pub fn delete(&mut self) {
        if self.pos < self.buf.len() {
            self.buf.remove(self.pos);
        }
    }

    pub fn left(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.pos < self.buf.len() {
            self.pos += 1;
        }
    }

    pub fn home(&mut self) {
        self.pos = 0;
    }

    pub fn end(&mut self) {
        self.pos = self.buf.len();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0;
    }

    pub fn take(&mut self) -> String {
        let s = self.text();
        self.clear();
        s
    }
}

/// A keypress, after the terminal has been translated into something meaningful.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// A line was submitted.
    Enter(String),
    /// Leave the REPL. Only from Ctrl-D or EOF, never from a single Ctrl-C.
    Quit,
    /// Stop what is happening: an empty Ctrl-C, or Ctrl-C with text to discard.
    ///
    /// A single Ctrl-C must never take the process with it. It is the key people press
    /// when something looks stuck, and a rescue tool that exits on it is refusing to do
    /// the one thing it is for -- worse, the session is the record of what was being
    /// fixed. Quitting takes `/exit`, Ctrl-D, or two Ctrl-C presses in quick succession.
    Interrupt,
    /// The input row changed and needs redrawing.
    Redraw,
    /// Nothing to do.
    Ignore,
}

/// Rows the streamed answer may occupy, above the input row.
///
/// This is the "viewport": a fixed-height strip at the bottom of the screen that
/// the answer is drawn into and that history insertion never touches. Codex calls
/// the same thing an inline viewport and arrives at the same design.
/// How long work must be running before the status line is worth showing.
const ACTIVITY_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

const ANSWER_ROWS: u16 = 3;
/// The row the running-status clock is drawn on: the one between the answer strip and
/// the input row.
///
/// It has to be its own row, because the clock is not part of the transcript machinery
/// and nothing arbitrates between it and the answer. Drawn inside the strip -- which is
/// where it used to go, on `input_row - 1` -- it shared that row with the answer's last
/// line, and the two simply wrote over each other: a clock repaint erased the answer's
/// final line, and an answer redraw erased the clock, so a long tool looked wedged.
const STATUS_ROWS: u16 = 1;
/// The input row, the status row and the answer strip.
const RESERVED: u16 = ANSWER_ROWS + STATUS_ROWS + 1;

/// Whether a debug build was asked to behave as if it had a terminal.
///
/// `FLINT_TERM_CAPTURE` makes a run take the interactive path with its output going
/// wherever stdout points, so the byte stream can be recorded and replayed. Release
/// builds do not compile the escape hatch at all: a stray variable must never be able
/// to change how a real run lays itself out.
pub fn capture_requested() -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var_os("FLINT_TERM_CAPTURE").is_some()
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

/// The file a captured run should write its bytes to, when it was given one.
///
/// `FLINT_TERM_CAPTURE` says "behave as if there were a terminal"; this says where the
/// bytes go. It exists because the alternative -- pointing the process's stdout at the
/// capture file -- also captures whatever else writes to stdout, which in a test binary
/// is the harness's own progress lines. One of those lands mid-capture, on the bottom
/// row, and its newline scrolls the transcript out of the recorded screen: a test that
/// fails with a blank screen and nothing wrong in the layout code to find. Writing to a
/// file of the run's own choosing removes the whole class.
pub fn capture_file() -> Option<std::path::PathBuf> {
    #[cfg(debug_assertions)]
    {
        std::env::var_os("FLINT_TERM_CAPTURE_FILE").map(std::path::PathBuf::from)
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// Where a run's bytes go, so the drawing code never asks which case it is in.
enum Sink<'a> {
    Stdout(std::io::Stdout),
    File(&'a std::fs::File),
}

impl std::io::Write for Sink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sink::Stdout(out) => out.write(buf),
            Sink::File(file) => file.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Stdout(out) => out.flush(),
            Sink::File(file) => file.flush(),
        }
    }
}

/// The words the status row uses for an activity, named or not.
///
/// One function because the terminal and the browser have to say the same thing: an unnamed
/// wait is `waiting for the model` on screen, and a stream carrying the raw empty name would
/// leave the browser with nothing to show during exactly the wait `--web` exists to make
/// visible.
fn activity_words(name: &str) -> String {
    if name.is_empty() {
        "waiting for the model".to_string()
    } else {
        name.to_string()
    }
}

/// What is running right now.
struct Activity {
    /// The tool name, or empty while the model is thinking rather than running one.
    name: String,
    /// When it started, as seconds since the process began.
    ///
    /// Elapsed time rather than a wall clock, so the status line is built from plain
    /// integers and a suspended machine does not make it jump.
    started: std::time::Instant,
}

/// `12s`, `1m 05s`, `1h 02m` -- short enough to sit in a status line.
fn elapsed_label(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub struct Term {
    interactive: bool,
    /// Last row of the screen (1-based, inclusive).
    screen_rows: AtomicU16,
    /// Screen width in columns, which is what decides where wrapped lines break.
    ///
    /// Needed because output is committed to history one *screen* row at a time: a
    /// single logical line longer than the screen occupies several, and counting it
    /// as one makes the transcript lose lines and repeat others.
    screen_cols: AtomicU16,
    /// Top row of the answer strip (1-based).
    viewport_top: AtomicU16,
    /// First row *below* the answer strip: the input row.
    input_row: AtomicU16,
    input: Mutex<Input>,
    /// Text before the input, e.g. "> " or "  name: ".
    prefix: Mutex<String>,
    /// Whether an answer is mid-stream and still has to be committed.
    stream_active: AtomicBool,
    /// The answer currently being streamed, so it can be finished off later.
    stream_text: Mutex<String>,
    /// How many of the streaming answer's lines have already gone to history.
    committed: AtomicU16,
    /// The full, unstripped text of the segment currently streaming.
    ///
    /// Needed because stripping and segment detection want opposite things from the same
    /// string. A model that resends the cumulative text puts us in a position where a
    /// fragment can start with the *committed head* rather than with the previous
    /// fragment -- that is precisely what makes it a repeat -- so once the head has been
    /// stripped, the stripped text is no longer a prefix of the next stripped text, and
    /// the boundary test fires mid-segment. Keeping the unstripped text here gives the
    /// boundary test the origin it needs, while `stream_text` holds what is being drawn.
    segment_text: Mutex<String>,
    /// The text of the segment most recently committed to history, so a later segment
    /// that repeats it at its head does not draw it a second time.
    ///
    /// A model that narrates, calls a tool, and then *resends the cumulative text*
    /// rather than only its continuation produces exactly that. Nothing in `committed`
    /// can tell us: it counts rows, and when the repeated head is short the answer never
    /// overflows the strip, so `committed` stays 0 while the head is nevertheless on
    /// screen. The result was the opening line printed twice -- once in the transcript
    /// and once at the head of the next segment, with no break before its continuation.
    ///
    /// Deliberately *not* derived from `stream_text`. That field holds the text the
    /// strip is drawing, which is this text minus the head; comparing a new fragment
    /// against it would compare two different origins, and the segment-boundary test
    /// would then fire on the very fragments the strip is meant to be dropping.
    last_segment_text: Mutex<String>,
    /// The row the next transcript line is written on.
    ///
    /// Transcript grows *downward* from the top of the history region, so the first line
    /// of a session lands on row 1 and the screen fills from the top. Writing at the
    /// region's bottom row instead -- which is what this used to do -- makes every line
    /// appear at the lowest row and scroll up from there, so a short session sits at the
    /// bottom of the window above a large blank area.
    ///
    /// It stops at `history_bottom`, because that is where a newline scrolls the region:
    /// beyond that the screen moves rather than the cursor.
    history_row: AtomicU16,
    /// Where the last frame's visible slice began, so the rows it used and this frame
    /// does not can be cleared.
    stream_first: AtomicU16,
    /// The rows the last frame drew into the answer strip.
    ///
    /// Needed to erase the tail of rows that have just got shorter. Clearing the
    /// whole row instead is what caused the answer to repeat its first line: a row
    /// filled to the exact width leaves the cursor in the *next* row, so the erase
    /// lands on a row that has not been drawn yet.
    stream_rows: Mutex<Vec<String>>,
    /// What is running right now, and since when.
    ///
    /// A tool can take minutes -- a build, a package download, a hung network call --
    /// and without a moving clock "still working" and "wedged" look identical from the
    /// outside. That is the difference between waiting and killing the thing.
    activity: Mutex<Option<Activity>>,
    /// The session working directory, so displayed paths can be shortened against it.
    ///
    /// Held as a plain string rather than a `PathBuf` because the only use is a prefix
    /// comparison against text the model wrote.
    cwd: Mutex<String>,
    /// Last value of the status clock that was painted, so the redraw only happens
    /// when the displayed number would actually change.
    activity_shown: AtomicU16,
    /// Whether the status line has been painted at least once for the current activity.
    ///
    /// The first paint waits out a short delay. Most turns produce their first token
    /// well within a second, and a status line that appears and vanishes that fast reads
    /// as a flicker rather than as information. Once the wait is long enough to notice,
    /// showing it is the whole point -- with streamed output buffered for retry safety
    /// there is no other sign that anything is happening.
    activity_painted: AtomicBool,
    /// When the last bare Ctrl-C arrived, so two in a row can mean quit.
    last_ctrl_c: Mutex<Option<std::time::Instant>>,
    /// Where a captured run writes, when it was given a file of its own.
    ///
    /// `None` is the ordinary case: the terminal, or -- for a captured run with no file
    /// named -- whatever the process's stdout points at. A shared reference is enough to
    /// write through (`impl Write for &File`), and taking no lock is deliberate: some
    /// drawing paths call other drawing paths, and a lock held across one of those is a
    /// deadlock. Interleaving is no worse than it already is on stdout.
    capture: Option<std::fs::File>,
}

impl Term {
    /// A terminal that does nothing but print.
    ///
    /// Used for one-shot mode, where the caller wants the answer and nothing
    /// else -- no reserved input row, no raw mode, no escape codes.
    pub fn plain() -> Self {
        Term {
            interactive: false,
            screen_rows: AtomicU16::new(24),
            screen_cols: AtomicU16::new(80),
            viewport_top: AtomicU16::new(20),
            input_row: AtomicU16::new(24),
            input: Mutex::new(Input::default()),
            prefix: Mutex::new("> ".to_string()),
            stream_active: AtomicBool::new(false),
            stream_text: Mutex::new(String::new()),
            committed: AtomicU16::new(0),
            history_row: AtomicU16::new(1),
            segment_text: Mutex::new(String::new()),
            last_segment_text: Mutex::new(String::new()),
            stream_rows: Mutex::new(Vec::new()),
            stream_first: AtomicU16::new(0),
            activity: Mutex::new(None),
            cwd: Mutex::new(String::new()),
            activity_shown: AtomicU16::new(0),
            activity_painted: AtomicBool::new(false),
            last_ctrl_c: Mutex::new(None),
            capture: None,
        }
    }

    /// Record the working directory, so transcript lines can show shorter paths.
    pub fn set_cwd(&self, cwd: &str) {
        *self.cwd.lock().unwrap() = cwd.trim_end_matches(['/', '\\']).to_string();
    }

    /// A path as the user would write it: relative to the session directory when it is
    /// inside it.
    ///
    /// The model usually hands over absolute paths, and a transcript of
    /// `/home/you/project/src/term.rs` twice per file operation is mostly prefix -- six
    /// lines of it look identical at a glance, which is the opposite of what a one-line
    /// summary is for.
    pub fn shorten_path(&self, raw: &str) -> String {
        let cwd = self.cwd.lock().unwrap();
        if cwd.is_empty() {
            return raw.to_string();
        }
        // Only strip at a separator boundary: `/app` must not shorten `/apple`.
        if let Some(rest) = raw.strip_prefix(cwd.as_str()) {
            if rest.is_empty() {
                return ".".to_string();
            }
            if let Some(rel) = rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\')) {
                return rel.to_string();
            }
        }
        raw.to_string()
    }

    /// Change what is shown before the input. Used by the configuration wizard,
    /// whose questions are not REPL commands.
    pub fn set_prefix(&self, prefix: &str) {
        *self.prefix.lock().unwrap() = prefix.to_string();
    }

    /// Enter raw mode and reserve the bottom row, if there is a console.
    pub fn start() -> Result<Self> {
        let tty = std::io::stdout().is_terminal();
        // A debug build can be pointed at a file instead of a terminal, which is the
        // only way to capture the real interactive byte stream for replay. Release
        // builds do not compile this at all, so it cannot be used to confuse a real
        // run. Raw mode and the window-size query still need a real terminal, so
        // those stay keyed on `tty` below.
        let interactive = tty || capture_requested();
        // Opened before anything is drawn, so the first frame is in the file too. A file
        // that cannot be opened is not worth failing a run over: the bytes then go to
        // stdout, exactly as they would have without the variable.
        let capture = capture_file().and_then(|path| {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::File::create(&path).ok()
        });

        let term = Term {
            interactive,
            capture,
            screen_rows: AtomicU16::new(24),
            screen_cols: AtomicU16::new(80),
            viewport_top: AtomicU16::new(20),
            input_row: AtomicU16::new(24),
            input: Mutex::new(Input::default()),
            prefix: Mutex::new("> ".to_string()),
            stream_active: AtomicBool::new(false),
            stream_text: Mutex::new(String::new()),
            committed: AtomicU16::new(0),
            history_row: AtomicU16::new(1),
            segment_text: Mutex::new(String::new()),
            last_segment_text: Mutex::new(String::new()),
            stream_rows: Mutex::new(Vec::new()),
            stream_first: AtomicU16::new(0),
            activity: Mutex::new(None),
            cwd: Mutex::new(String::new()),
            activity_shown: AtomicU16::new(0),
            activity_painted: AtomicBool::new(false),
            last_ctrl_c: Mutex::new(None),
        };

        // Raw mode and the panic hook need a real console; laying out the screen does
        // not. A capture therefore runs `reclaim`/`setup`/`redraw` too, which is the
        // point: the startup path -- scrolling the old screen away and clearing what is
        // left -- is exactly where "the previous screen is still visible" lives, and it
        // was untestable while it only ran against a terminal.
        if tty {
            // A panic with raw mode on leaves the user without echo, which looks
            // like a broken shell. Restore first, then let the message print.
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let _ = disable_raw_mode();
                // Deliberately not the term's own sink: this runs from the panic hook,
                // which outlives the `Term` it was installed by, and a panic path must
                // not be the thing that fails.
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b[r\x1b[?25h\x1b[?2004l\r\n");
                let _ = out.flush();
                default_hook(info);
            }));

            enable_raw_mode()?;
            // Bracketed paste, and this is the whole reason a paste works at all.
            //
            // Without it the terminal sends the pasted text one keystroke at a time, so
            // every newline in it is an Enter: a three-line paste became three messages,
            // and the second and third arrived while the first turn was running, which is
            // the steering path -- so they *interrupted* it. Reported from a real session
            // as "the model treats it as one sentence", which is what it looks like from
            // the outside: only the first line was ever its message.
            //
            // The handler for `Event::Paste` below has been in this file all along and had
            // never once run in a real terminal, because nothing asked the terminal to send
            // the markers it depends on.
            let _ = execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
        }
        if interactive {
            term.reclaim();
            term.reserve_screen();
            term.redraw();
        }

        Ok(term)
    }

    /// Note that something started, so the status line can say what is being waited for.
    ///
    /// Returns whether the clock actually **started over**, which is not the same as
    /// "this was called": a repeated name keeps the original start time, deliberately, so
    /// a tool that reports itself in stages does not reset its own clock and never appear
    /// to take long. The return value exists for the second renderer -- the browser behind
    /// `--web` -- which has its own clock and has to start it over at exactly the moments
    /// this one does. It is answered even when there is no terminal, because a browser
    /// watching a piped run still needs to be told.
    pub fn activity_started(&self, name: &str) -> bool {
        // Keep the original start time for a repeated name: a tool that reports itself
        // in stages would otherwise reset its own clock and never appear to take long.
        let started_over = {
            let mut activity = self.activity.lock().unwrap();
            let keep = matches!(&*activity, Some(a) if a.name == name);
            if !keep {
                *activity = Some(Activity {
                    name: name.to_string(),
                    started: std::time::Instant::now(),
                });
                // A sentinel, not zero. `tick` repaints only when the displayed second
                // changes, and the first second of any activity *is* zero -- so starting
                // from zero means the very first paint is suppressed as a no-op, and the
                // status line cannot appear until the clock reaches one second. That is the
                // whole window in which a fast-but-not-instant turn needs to say something.
                self.activity_shown.store(u16::MAX, Ordering::Relaxed);
                self.activity_painted.store(false, Ordering::Relaxed);
            }
            !keep
        };
        started_over
    }

    /// Rename what is running without restarting its clock.
    ///
    /// Used when a wait turns into something more specific -- the model is no longer
    /// being waited for, it is thinking -- and the elapsed time should keep counting
    /// from when the wait began rather than from the moment the name changed.
    ///
    /// Returns whether anything changed. `false` means the caller should say nothing to a
    /// second renderer either: a rename that did not happen is not news.
    pub fn activity_named(&self, name: &str) -> bool {
        let renamed = {
            let mut activity = self.activity.lock().unwrap();
            match activity.as_mut() {
                Some(a) if a.name != name => {
                    a.name = name.to_string();
                    true
                }
                // Nothing running: naming it would invent an activity out of a stray
                // fragment, and a clock with no start time has nothing to show.
                _ => false,
            }
        };
        if renamed && self.interactive {
            // Repaint now rather than at the next second: the point of the change is
            // that the line said the wrong thing until it was corrected. Only if it is
            // already on screen, though -- see `repaint_if_visible`.
            self.repaint_if_visible();
        }
        renamed
    }

    /// The words the status row is showing, or an empty string when nothing is running.
    ///
    /// The browser behind `--web` is told exactly this, rather than the name the caller
    /// passed. The two are not the same: an empty name is shown as `waiting for the model`,
    /// and a renderer given the raw name would have to invent that phrase -- or, worse,
    /// blank its status line at the exact moment the wait began.
    pub fn activity_label(&self) -> String {
        let activity = self.activity.lock().unwrap();
        activity.as_ref().map(|a| activity_words(&a.name)).unwrap_or_default()
    }

    /// Whether the activity is still the initial wait, with no name yet.
    ///
    /// The status line is the only place that knows what phase the turn is in, so the
    /// caller asks it rather than keeping a second copy of the same fact.
    pub fn activity_is_unnamed_wait(&self) -> bool {
        self.activity
            .lock()
            .unwrap()
            .as_ref()
            .map(|a| a.name.is_empty())
            .unwrap_or(false)
    }

    /// Show a line of a running command's output on the status row.
    ///
    /// Replaces the activity's *name* rather than adding a line, so a download updating
    /// its percentage does not scroll the transcript. The clock keeps running: the turn
    /// has been going since it started, whatever the last line says.
    pub fn activity_detail(&self, detail: &str) {
        if !self.interactive {
            return;
        }
        let shown = {
            let mut activity = self.activity.lock().unwrap();
            match activity.as_mut() {
                Some(a) => {
                    // Trimmed to what the row can hold: a status line that wraps is worse
                    // than one that is cut, because the wrap lands on the input row.
                    let cols = self.screen_cols.load(Ordering::Relaxed) as usize;
                    let room = cols.saturating_sub(24).max(20);
                    let mut text = detail.to_string();
                    if text.chars().count() > room {
                        text = text.chars().take(room.saturating_sub(1)).collect::<String>();
                        text.push('\u{2026}');
                    }
                    if a.name == text {
                        false
                    } else {
                        a.name = text;
                        true
                    }
                }
                None => false,
            }
        };
        if shown {
            self.repaint_if_visible();
        }
    }

    /// Repaint the status row, but only if it is showing something already.
    ///
    /// The hold-back in `tick` exists so an activity that is over in a moment never
    /// appears at all. A rename or a progress line arrives inside that window, and
    /// painting it immediately is what put `── 0s writing the answer ──` on screen for a
    /// model that answered in fifty milliseconds -- the fast tool's fault, one line up.
    /// The next tick draws it with whatever name it has by then, which is the point of
    /// holding back in the first place.
    fn repaint_if_visible(&self) {
        if self.activity_painted.load(Ordering::Relaxed) {
            self.paint_activity();
        }
    }

    /// Where the bytes of this run go: the terminal, or the capture file.
    ///
    /// Every drawing path goes through here, so a captured run does not depend on the
    /// process's stdout pointing anywhere in particular -- see `capture_file`.
    fn sink(&self) -> Sink<'_> {
        match &self.capture {
            Some(file) => Sink::File(file),
            None => Sink::Stdout(std::io::stdout()),
        }
    }

    /// How long the current activity has been running; zero when none is.
    ///
    /// Lets the caller tell a slow model from one that is not answering. The clock itself
    /// cannot make that distinction -- only elapsed time can.
    pub fn activity_elapsed(&self) -> std::time::Duration {
        self.activity
            .lock()
            .unwrap()
            .as_ref()
            .map(|a| a.started.elapsed())
            .unwrap_or_default()
    }

    /// The tool finished: stop the clock.
    pub fn activity_done(&self) {
        if !self.interactive {
            return;
        }
        *self.activity.lock().unwrap() = None;
        self.activity_shown.store(0, Ordering::Relaxed);
        self.paint_activity();
    }

    /// Repaint the status line when its number would change.
    ///
    /// Called from the REPL's own waiting loop, which is already ticking every couple
    /// of milliseconds. A thread of its own would need the `Term` to outlive the
    /// caller, and the input loop is right there -- one comparison while nothing is
    /// running, and a repaint four times a second while something is.
    pub fn tick(&self) {
        if !self.interactive {
            return;
        }
        let elapsed = {
            let activity = self.activity.lock().unwrap();
            match activity.as_ref() {
                Some(a) => a.started.elapsed(),
                None => return,
            }
        };
        // Hold the first paint back briefly. A status line that appears and disappears
        // inside half a second is a flicker, not information; anything slower than that
        // is exactly when the user needs to see that work is happening.
        if !self.activity_painted.load(Ordering::Relaxed) {
            if elapsed < ACTIVITY_DELAY {
                return;
            }
            self.activity_painted.store(true, Ordering::Relaxed);
        }
        let seconds = elapsed.as_secs() as u16;
        if seconds == self.activity_shown.load(Ordering::Relaxed) {
            return;
        }
        self.activity_shown.store(seconds, Ordering::Relaxed);
        self.paint_activity();
    }

    /// Draw, or clear, the running-status line in the row just above the input.
    fn paint_activity(&self) {
        if !self.interactive {
            return;
        }
        let row = self.input_row.load(Ordering::Relaxed).saturating_sub(1);
        let line = {
            let activity = self.activity.lock().unwrap();
            activity.as_ref().map(|a| {
                let what = activity_words(&a.name);
                format!(
                    "\u{2500}\u{2500} {} {} \u{2500}\u{2500}",
                    elapsed_label(a.started.elapsed()),
                    what
                )
            })
        };
        let mut out = self.sink();
        // The whole row is cleared either way, so a finished tool leaves no stale clock
        // behind on a row that is also part of the answer strip.
        let _ = write!(out, "\x1b[{};1H\x1b[2K", row);
        if let Some(line) = line {
            let width = display_width(&line);
            let cols = self.screen_cols.load(Ordering::Relaxed);
            let pad = cols.saturating_sub(width) / 2;
            let _ = write!(out, "\x1b[{};{}H\x1b[2m{line}\x1b[0m", row, pad.max(1));
        }
        let _ = write!(out, "\x1b[?25l");
        let _ = out.flush();
    }

    pub fn interactive(&self) -> bool {
        self.interactive
    }

    pub fn input_mut(&self) -> std::sync::MutexGuard<'_, Input> {
        self.input.lock().unwrap()
    }

    /// Recompute the layout from the current window size.
    fn reclaim(&self) {
        let (w, h) = size().unwrap_or((80, 24));
        // A test has no terminal to ask, and a capture made at whatever size the
        // harness happened to report cannot be replayed at a different one: lines wrap
        // in different places and every assertion about them becomes a guess. Debug
        // builds only, and it changes nothing about how the code lays itself out.
        //
        // Shadowed rather than reassigned, so a release build -- where the override is
        // compiled out entirely and the binding would never be written to -- does not
        // warn about a `mut` that only the debug configuration needs.
        #[cfg(debug_assertions)]
        let (w, h) = match std::env::var_os("FLINT_TERM_SIZE").and_then(|spec| {
            let (a, b) = spec.to_str()?.split_once('x')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        }) {
            Some((cw, ch)) => (cw, ch),
            None => (w, h),
        };
        let h = h.max(RESERVED + 1);
        self.screen_rows.store(h, Ordering::Relaxed);
        self.screen_cols.store(w.max(20), Ordering::Relaxed);
        self.input_row.store(h, Ordering::Relaxed);
        self.viewport_top.store(h - ANSWER_ROWS, Ordering::Relaxed);
    }

    /// Take the screen: push the old one into the scrollback, blank it, and start the
    /// transcript at the top.
    ///
    /// **Once, at startup.** It scrolls and clears everything and rewinds the transcript
    /// to row 1, so running it again -- which the resize handler used to do -- wipes the
    /// conversation off the screen. Resizing needs `redraw`, not this.
    fn reserve_screen(&self) {
        if !self.interactive {
            return;
        }
        let mut out = self.sink();
        let rows = self.screen_rows.load(Ordering::Relaxed);
        // Take the screen, rather than inheriting wherever the shell left the cursor.
        //
        // Transcript is written at absolute rows so it can grow downward from row 1, and
        // that only stays honest if those rows are ours: writing at row 1 of a screen
        // still holding a shell prompt would overwrite the prompt instead of scrolling it
        // away. Scrolling first pushes the existing screen into the scrollback, where it
        // is still readable, and leaves the cursor at the top of a screen we own.
        let _ = write!(out, "\x1b[1;{}r", rows);
        for _ in 0..RESERVED {
            let _ = write!(out, "\x1bM");
        }
        let _ = write!(out, "\x1b[r");
        // Every row, not just the transcript's.
        //
        // The scrolls above moved the shell's screen *down* into the rows the status
        // line, the answer strip and the input line occupy. Those rows are repainted in
        // place rather than scrolled, so anything left there stays on screen for the
        // whole session, sitting immediately above and beside the input line. Clearing
        // only rows 1..history_bottom is what allowed that -- reported from a real
        // session as "a bracket to the left and right of the command line that does not
        // go away until you scroll".
        for row in 1..=rows {
            let _ = write!(out, "\x1b[{};1H\x1b[2K", row);
        }
        self.history_row.store(1, Ordering::Relaxed);
        // Force a full repaint rather than trusting the line erases above.
        //
        // Erasing a line rewrites its *characters*, which is not the same as repainting
        // the screen: anything the terminal had already rasterised and not yet
        // invalidated survives, showing as stale glyphs in the gaps -- reported as
        // brackets sitting at the left and right edges of a row, absent from the text
        // buffer, and vanishing the moment the window was resized. A resize works
        // because it makes the terminal rebuild its whole display, which is exactly what
        // this asks for up front.
        //
        // `2J` clears the display (not the scrollback) and `H` homes the cursor; the
        // banner is then drawn on a screen the terminal has fully re-rendered.
        let _ = write!(out, "\x1b[2J\x1b[1;1H");
        let _ = out.flush();
    }

    /// Last row of the answer strip: the row above the status line.
    ///
    /// Not `input_row - 1`. That row belongs to the running-status clock, and the strip
    /// stops above it -- which is what keeps a clock repaint and an answer redraw from
    /// writing over one another.
    fn answer_bottom(&self) -> u16 {
        self.input_row
            .load(Ordering::Relaxed)
            .saturating_sub(1 + STATUS_ROWS)
    }

    /// Row the history region ends on: the row above the answer strip.
    fn history_bottom(&self) -> u16 {
        self.viewport_top.load(Ordering::Relaxed).saturating_sub(1).max(1)
    }

    /// Write one complete line of output, then put the input row back.
    ///
    /// Takes `Arguments` so that call sites can stay `println!`-shaped: the
    /// alternative is `term.line(&format!(...))` at seventy places, which is
    /// seventy chances to get a bracket wrong.
    pub fn line(&self, args: std::fmt::Arguments<'_>) {
        if !self.interactive {
            println!("{args}");
            return;
        }
        self.close_stream();
        self.emit_history(&format!("{args}"));
    }

    /// Say something that is not part of the conversation.
    ///
    /// Goes through the transcript machinery rather than straight to stderr, because a
    /// stray write lands wherever the cursor is -- inside the answer strip -- and tears
    /// the layout apart. `line` commits the half-written answer first and the next
    /// fragment redraws the strip, so the notice reads as a line above a continuing
    /// answer rather than overwriting one.
    ///
    /// Non-interactive differs on purpose: the answer is on stdout and has to stay
    /// parseable, so a notice goes to stderr even though the two share a terminal.
    pub fn notice(&self, message: &str) {
        if !self.interactive {
            eprintln!("flint: {message}");
            return;
        }
        self.line(format_args!("{message}"));
    }

    /// An empty line. `format_args!()` is not a valid format string, so a bare
    /// `println!()` needs its own path.
    pub fn blank(&self) {
        self.line(format_args!(""));
    }

    /// Write text owned by a subprocess, adding a newline only if it lacks one.
    ///
    /// `!cmd` and `flint exec` must reproduce the command's bytes, and adding a
    /// newline to output that already has one puts a blank line between every
    /// pair of commands.
    pub fn text_ln(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Each line goes through `line`, so a multi-line result is placed the
        // same way as any other output.
        for part in text.trim_end_matches(['\n', '\r']).split('\n') {
            self.line(format_args!("{}", part.trim_end_matches('\r')));
        }
    }

    /// Finish the answer in progress, so the next line starts below it.
    ///
    /// Does nothing when nothing is streaming: that is the common case, and it
    /// keeps `blank()` from inserting a second blank line.
    pub fn end_stream(&self) {
        if !self.interactive {
            println!();
            return;
        }
        self.close_stream();
    }

    /// Start a new answer, ending any that is still in progress.
    ///
    /// `run_turn` accumulates one answer per turn and streams the whole of it on each
    /// fragment, so the strip recognises a restatement by comparing the incoming text
    /// against the running record of what this answer has already put in the transcript.
    /// That record belongs to one turn and has to be dropped when the next one starts.
    ///
    /// Left over, it never matches again -- the new turn's text begins at nothing, not at
    /// the end of the previous answer -- so no head is ever stripped and every round of
    /// the new turn commits everything said so far, one longer block per tool round:
    ///
    /// ```text
    /// > ask something else
    /// Looking at the config first.
    /// ✓ bash
    /// Looking at the config first. It has not changed, so the key is still missing.
    /// ```
    ///
    /// A process's first turn is unaffected, because its record starts empty, and that is
    /// why the fault only ever appeared on the second turn and later.
    pub fn begin_answer(&self) {
        if !self.interactive {
            return;
        }
        // Anything still streaming is real output, so it is committed before the record
        // it contributed to is thrown away.
        self.close_stream();
        self.committed.store(0, Ordering::Relaxed);
        self.stream_rows.lock().unwrap().clear();
        self.last_segment_text.lock().unwrap().clear();
    }

    /// Insert history lines `lines` above the answer strip.
    ///
    /// This is Codex's inline-viewport idea, and it is the whole reason the layout
    /// stays stable. The scroll region is narrowed to the rows *above* the strip:
    ///
    /// ```text
    /// row 1
    ///   :     <- scroll region 1..V-1: history lands here and scrolls up through it
    /// row V-1
    /// row V   <- answer strip (the "viewport"): never part of the region
    /// row V+3
    /// row V+4 <- input row
    /// ```
    ///
    /// Narrowed like that, a newline on the region's bottom row scrolls history up
    /// and leaves the strip completely alone -- it never has to be redrawn or
    /// repositioned, and no cursor position has to be tracked between writes. The
    /// region is only set for the duration of the write and then reset, so a stray
    /// scroll can never reach the strip or the input row.
    /// Write lines into the transcript above the strip.
    ///
    /// Lines are wrapped to the screen width first, so exactly one terminal row is
    /// produced per item. Writing a longer line would make the terminal wrap it inside
    /// the scrolling region, and the continuation would then run past the region's
    /// bottom margin -- the same row written over and over down the screen. Doing the
    /// wrapping here keeps the row count honest for every caller.
    fn insert_history(&self, lines: &[String]) {


        let mut wrapped: Vec<String> = Vec::new();
        for line in lines {
            let cols = self.screen_cols.load(Ordering::Relaxed);
            wrapped.extend(wrap_rows(line, cols));
        }
        if wrapped.is_empty() {
            return;
        }
        let bottom = self.history_bottom();
        let mut out = self.sink();
        for text in &wrapped {
            // Where this line goes, then advance. At the bottom the newline scrolls the
            // region instead, so the row stays put and the *screen* moves -- which is
            // what keeps a full transcript scrolling rather than overwriting itself.
            let row = self.history_row.load(Ordering::Relaxed).clamp(1, bottom);
            let _ = write!(out, "\x1b[1;{}r", bottom);
            let _ = write!(out, "\x1b[{};1H\r\x1b[2K{text}\r\n", row);
            let _ = write!(out, "\x1b[r");
            self.history_row
                .store(row.saturating_add(1).min(bottom), Ordering::Relaxed);
        }
        let _ = write!(out, "\x1b[?25l");
        let _ = out.flush();
        self.redraw();
    }

    /// One line of committed output.
    ///
    /// The transcript is written inside the scroll region, which stops one row above the
    /// answer strip, so nothing here can disturb the status line: it used to clear and
    /// repaint that row on every committed line, on the theory that inserting a row
    /// pushed the clock up the screen. It cannot -- the region it scrolls is above both
    /// -- and the repaint was drawn without waiting out `ACTIVITY_DELAY`, which is what
    /// put a `── 0s read ──` on screen for every tool fast enough that the wait was not
    /// supposed to show a clock at all.
    fn emit_history(&self, text: &str) {
        if !self.interactive {
            return;
        }
        self.insert_history(&[text.to_string()]);
    }

    /// Render a streamed answer into the answer strip.
    ///
    /// The accumulated text is redrawn in full on every fragment, which is what
    /// makes a stream that splits mid-word read as one continuous line: each
    /// fragment re-renders the whole answer, so the terminal always shows the
    /// complete text rather than the last fragment alone.
    ///
    /// The strip is a *fixed slice* sitting on the input row and it never moves, so
    /// nothing done here can disturb the transcript above it -- that is the whole
    /// point of the layout. The answer is drawn bottom-anchored inside the slice;
    /// lines that no longer fit at its top are handed to history in order, so the
    /// transcript keeps the whole answer even though the slice shows only its tail.
    pub fn stream(&self, text: &str) {
        // Not a terminal: no strip, no cursor games, just the bytes. A notice during a
        // piped run goes to stderr so the answer on stdout stays parseable.
        if !self.interactive {
            print!("{text}");
            let _ = std::io::stdout().flush();
            return;
        }
        self.stream_active.store(true, Ordering::Relaxed);

        // A model that resends the *cumulative* text rather than only the continuation
        // hands us a segment whose head is already in the transcript. Drawing it whole
        // printed the opening line a second time -- once in the transcript, where it went
        // when the tools ran, and once at the head of the new segment, with no break
        // before its own continuation, because the redraw starts at column 1 of the
        // strip's top row rather than after the committed line:
        //
        //   I'll read both files.`src/lib.rs`: library root declaring nine modules...
        //
        // So drop the committed head first, and let everything downstream -- the strip's
        // text, `committed`, and `close_stream` -- work on what is left. They must all
        // agree on one text: `committed` counts rows of *the text being drawn*, and
        // `close_stream` commits from it, so leaving `stream_text` holding the unstripped
        // text applies one text's row offsets to another's rows.
        let incoming = text;

        let stripped;
        let text = {
            let head = self.last_segment_text.lock().unwrap();

            if !head.is_empty() {
                // Leading blank space is not new content, and a model that restates
                // itself habitually separates the restatement from what follows with
                // one. Comparing from the first non-blank character is what keeps a
                // restatement recognisable when it does not begin at column one.
                let lead = text.len() - text.trim_start().len();
                if text[lead..].starts_with(head.as_str()) {
                    stripped = text[lead + head.len()..].to_string();
                    stripped.as_str()
                } else {
                    text
                }
            } else {
                text
            }
        };

        {
            // The boundary test uses the *unstripped* text, and it has to: the fragment
            // that begins a repeat does not start with the previous fragment at all -- it
            // starts with the committed head instead. Comparing the stripped form would
            // fire a segment reset on exactly the fragments the strip is dropping.
            let held = self.segment_text.lock().unwrap().clone();

            // Within one streamed answer the text only ever grows, so text that does not
            // start with what came before is a *new segment*: the reasoning is over and
            // the answer has begun, or the answer resumed after a tool finished.
            //
            // A new segment has to start from an empty strip. It used to keep the old
            // rows, so each round's narration stayed on screen under the next one and the
            // same sentence was visible once per tool round. The previous segment is
            // committed first, because it is real output and must not be dropped.
            let fresh_segment = !incoming.starts_with(held.as_str());
            // An empty previous text is the first fragment of the turn, not a segment
            // boundary, and the strip is already blank.
            let after_something = !held.is_empty();

            if fresh_segment && after_something {
                self.close_stream();
                self.clear_viewport();
                self.committed.store(0, Ordering::Relaxed);
                self.stream_rows.lock().unwrap().clear();
            }
            // What is recorded is the text being *drawn*, not the text as it arrived.
            //
            // These differ whenever a segment restates what is already in the transcript:
            // the head is stripped for drawing, and `close_stream` commits from this same
            // variable. Storing the unstripped text made every tool round commit the whole
            // accumulation again, so the transcript read:
            //
            //   I'll see how it was installed.
            //   I'll see how it was installed. Let me check the update options.
            //   I'll see how it was installed. Let me check the update options. Confirmed.
            //
            // `segment_text` keeps the unstripped form, because the segment-boundary test
            // needs an origin that stays monotonic -- the stripped form shrinks when a
            // restatement is removed, and comparing against it fired a reset mid-segment.
            *self.stream_text.lock().unwrap() = text.to_string();
            *self.segment_text.lock().unwrap() = incoming.to_string();
        }

        // Rows are *screen* rows, not lines. A line longer than the screen is several
        // of them, and every count below -- what fits in the slice, what has to go to
        // history -- is a count of screen rows, because that is what the terminal
        // scrolls and what the slice can hold.
        let rows = wrap_rows(text, self.screen_cols.load(Ordering::Relaxed));
        let height = rows.len() as u16;
        let top = self.viewport_top.load(Ordering::Relaxed);
        let last = self.answer_bottom();
        let capacity = last.saturating_sub(top).saturating_add(1);

        // The last row is the one still being appended to: a fragment that arrives next
        // can rewrap it and change where it breaks. Everything above it is final, and
        // only final rows may be committed -- handing a row to history that later
        // rewraps would leave a duplicate of it behind on screen.
        let settled = if text.ends_with('\n') {
            height
        } else {
            height.saturating_sub(1)
        };

        // Every row of the slice is accounted for, so a row that leaves the top of the
        // slice must be handed to history or it would be lost when the slice scrolls
        // it away.
        let committed = self.committed.load(Ordering::Relaxed);
        if height > capacity {
            let drop = (height - capacity).min(settled);
            if drop > committed {
                let fresh: Vec<String> = rows[committed as usize..drop as usize].to_vec();
                self.insert_history(&fresh);
                self.committed.store(drop, Ordering::Relaxed);
            }
        }

        // Draw from the first row that has *not* been committed, not from the overflow
        // boundary. The two differ after a segment reset -- a tall answer has had rows
        // committed, the next segment shrinks the text, so the boundary moves back below
        // what is already in the transcript -- and drawing from the boundary then puts
        // those committed rows back on screen, directly beneath a transcript that already
        // has them.
        let committed_now = self.committed.load(Ordering::Relaxed) as usize;
        let first = (height.saturating_sub(capacity) as usize).max(committed_now);
        let visible = &rows[first.min(rows.len())..];
        let start = top + capacity.saturating_sub(visible.len() as u16);
        let mut out = self.sink();
        let previous = std::mem::take(&mut *self.stream_rows.lock().unwrap());
        let previous_first = self.stream_first.swap(first as u16, Ordering::Relaxed) as usize;

        let previous_drawn = previous.len();
        // A row the previous frame drew starts where its slice started, which is the top
        // of the strip plus whatever padding its length required.
        let previous_start_frame =
            top + capacity.saturating_sub(previous_drawn.min(capacity as usize) as u16);

        // Which row of the answer each strip row held last time, compared per row rather
        // than by "did the slice move".
        //
        // The slice can lose its *first* row without its start changing at all: the text
        // is cumulative, so as the answer grows the visible window slides while the row it
        // is drawn on stays put. A row that is no longer part of the slice has to be
        // erased, or the earlier part of the answer sits in the strip while the rest draws
        // underneath it -- the same sentence, visible twice, once just above its own
        // continuation.
        //
        // Only rows the previous frame actually drew are considered: the rows below it
        // were erased then, and the ones above belong to history.
        //
        // **Unless the slice slid**, in which case the terminal is asked to move those cells
        // instead. The window sliding is the common case for a long answer, and redrawing
        // every row one position up is what made 64 deltas cost 3.4x the answer (ROADMAP §6).
        // `CSI S` scrolls the strip's *own* region, so nothing above it moves, and the rows
        // that scroll out at the top are the ones already handed to history a few lines up.
        // The scroll is only valid when the rest of the block is what it was: the window must
        // have moved by the same amount for every row, and one frame's worth of the block --
        // its last row, the one still being appended to -- is allowed to differ, because that
        // row is not final in the older frame.
        let slid = first.saturating_sub(previous_first);
        let overlap = visible.len().saturating_sub(slid);
        let can_slide = slid > 0
            && previous_drawn == visible.len()
            && start == previous_start_frame
            && slid <= previous.len()
            && visible[..overlap.saturating_sub(1)]
                .iter()
                .zip(previous[slid..].iter())
                .all(|(now, before)| now == before);
        if can_slide {
            let _ = write!(out, "\x1b[{};{}r\x1b[{}S\x1b[r", top, last, slid);
        } else {
            for n in 0..previous_drawn {
                let strip_row = previous_start_frame + n as u16;
                if strip_row < top || strip_row > last {
                    continue;
                }
                let was_showing = previous_first + n;
                let still_showing = was_showing >= first && was_showing < first + visible.len();
                if !still_showing {
                    let _ = write!(out, "\x1b[{};1H\x1b[2K", strip_row);
                }
            }
        }
        // Rewrite each visible row in place, then erase whatever the row held before
        // and no longer needs.
        //
        // Clearing the whole row first would be simpler, and is wrong. When a line
        // fills the width exactly, the terminal leaves the cursor in the *next* row,
        // so the next write's `CR` and erase land on a row that has not been drawn
        // yet and wipes it. The answer then grows by repeating its first row, which
        // is precisely the fault this streaming code exists to avoid. Writing the
        // text first and erasing only the tail never touches a row ahead of the
        // cursor.
        let mut drawn = 0u16;
        // The `\r\n` that carries the cursor to the next row belongs to the row that was
        // *written*, and only to it. Emitted after a row that nothing was written for, it
        // would move the cursor from wherever it was already parked -- the input row, since
        // that is the last thing drawn before this -- and a line feed on the screen's last
        // row scrolls the *whole* transcript up by one. That is not a hypothetical: it is
        // what a `\r\n` after a skipped row did, and the failure looked like the answer
        // being eaten a row at a time, with history developing holes.
        let more = visible.len().saturating_sub(1);
        for (n, line) in visible.iter().enumerate() {
            let r = start + n as u16;
            if r > last {
                break;
            }
            // What this *screen row* held last frame, if it held anything. Matched by screen
            // row rather than by position in the slice: the slice slides as the answer grows,
            // so the same index can be a different row, and a difference taken against the
            // wrong row would skip a row that did change -- leaving stale text on screen.
            // Rows outside the previous frame's range were blank, so nothing to compare to.
            //
            // `slid` is the correction for the scroll above: the cells on this row are the
            // ones that were `slid` rows further down before it, so that is what they have to
            // be compared against. It is zero whenever no scroll was emitted -- the terminal
            // does not move anything by itself, and comparing against a row that is not there
            // would skip a row that did change.
            let base = if can_slide { slid as u16 } else { 0 };
            let previous_here = r
                .checked_sub(previous_start_frame)
                .map(|index| index.saturating_add(base))
                .and_then(|index| previous.get(index as usize));
            match previous_here {
                // Already on screen exactly as it should be. Writing it again is the whole
                // cost this rewrite exists to remove: at 64 deltas the old painter drew 3.4x
                // the answer, and nearly all of it was rows re-saying what they already said
                // (ROADMAP §6, `measured_cost_of_streaming_an_answer`).
                //
                // Nothing else writes inside the strip between frames: `insert_history`
                // scrolls only the region above it, and `clear_viewport` clears
                // `stream_rows` along with the rows it erases.
                Some(previous_row) if previous_row == line => {}
                Some(previous_row) => {
                    // The common case by far: the row kept its head and grew at the end.
                    // Only the part from the first difference on is sent, positioned at the
                    // column that difference starts in -- the head is already on screen.
                    let keep = common_prefix_len(previous_row, line);
                    let at = display_width(&line[..keep]);
                    let _ = write!(out, "\x1b[{};{}H{}", r, at + 1, &line[keep..]);
                    let width = display_width(line);
                    let previous_width = display_width(previous_row);
                    if previous_width > width {
                        // `CSI {n}K` erases n cells from the cursor without moving it, so
                        // it shortens the row and leaves the cursor where the next line's
                        // positioning needs it to be.
                        let _ = write!(out, "\x1b[{}K", previous_width - width);
                    }
                    if n < more {
                        // A short line needs the `CR` to return from wherever it ended; a
                        // full one is already at column 1 of the next row.
                        let _ = write!(out, "\r\n");
                    }
                }
                None => {
                    let _ = write!(out, "\x1b[{};1H{line}", r);
                    if n < more {
                        let _ = write!(out, "\r\n");
                    }
                }
            }
            drawn += 1;
        }
        for r in (start + drawn)..=last {
            let _ = write!(out, "\x1b[{};1H\x1b[2K", r);
        }

        // Park the cursor immediately after the last visible line's text, where the
        // next fragment continues from.
        if let Some(last_line) = visible.last() {
            let r = start + (visible.len() as u16).saturating_sub(1);
            if r <= last {
                let w = display_width(last_line);
                // A line as wide as the screen already left the cursor on the row
                // below, so aiming at column 1 is what keeps the next fragment there.
                let _ = write!(out, "\x1b[{};{}H", r, w.max(1));
            }
        }
        let _ = write!(out, "\x1b[?25l");
        let _ = out.flush();
        // Remember this frame, so the next one knows what it has to shorten.
        *self.stream_rows.lock().unwrap() = visible.iter().map(|s| s.to_string()).collect();
        self.redraw();
    }

    /// The streamed answer is finished: hand it to history and clear the slice.
    ///
    /// Order matters here. The lines still on screen go to history *first*, so
    /// nothing can be lost, and the slice is blanked *before* it is scrolled, so
    /// every row the scroll touches is already empty. Doing it the other way round
    /// loses the answer's last line and duplicates another.
    fn close_stream(&self) {
        if !self.stream_active.swap(false, Ordering::Relaxed) {
            return;
        }
        // Take the text rather than copy it: this runs more than once per turn -- a tool
        // result arriving mid-turn closes the current segment, and the next segment's
        // first fragment closes it again -- and if the text stayed behind, the second run
        // would commit the same rows a second time. Taking it makes the function
        // idempotent, which is what its callers already assume.
        let text = std::mem::take(&mut *self.stream_text.lock().unwrap());

        if text.is_empty() {
            return;
        }
        // Screen rows again: the slice holds `capacity` of them, and the rows that slid
        // off its top were committed as they went, so only what is *still* on screen is
        // left to hand over.
        //
        // "Still on screen" is the tail, but "not yet committed" is what must be sent,
        // and those are not the same set: a tall answer has had most of its rows
        // committed already. Committing the tail by position -- which is what this used
        // to do -- sends the overlap a second time, and that is the reported fault of
        // each round's text appearing again underneath the next one.
        let lines = wrap_rows(&text, self.screen_cols.load(Ordering::Relaxed));
        let top = self.viewport_top.load(Ordering::Relaxed);
        let last = self.answer_bottom();
        let capacity = last.saturating_sub(top).saturating_add(1) as usize;
        let shown = lines.len().min(capacity);
        let sent = (self.committed.load(Ordering::Relaxed) as usize).min(lines.len());
        let remaining: Vec<String> = lines[sent..].to_vec();
        if !remaining.is_empty() {
            self.insert_history(&remaining);
            self.committed
                .store(lines.len().min(u16::MAX as usize) as u16, Ordering::Relaxed);
        }
        // Record what is now in the transcript, so a later segment that restates it can
        // drop the repeat instead of drawing a second copy.
        //
        // The *committed part*, not the whole text. The text handed in is the answer as
        // it stands, and rows below `committed` may not have gone to history at all --
        // they are still in the strip, and they will be handed over by a later close. The
        // next round restates this round, so recording the whole thing claims that rows
        // which never reached the transcript are already in it, the rows get shown twice,
        // and each round's line is longer than the last:
        //
        //   I'll see how it was installed.
        //   I'll see how it was installed. Let me check the update options.
        //   I'll see how it was installed. Let me check the update options. Confirmed.
        //
        // Which is what a real eight-round session looked like.
        // Appended, not replaced: this is the running prefix of everything handed to the
        // transcript, and a restatement is compared against all of it.
        //
        // Storing only this segment's text is the tempting version and it is wrong,
        // because the model sends the *accumulation*, not the addition. Round one commits
        // "A" and records it; round two arrives as "A B" and commits only " B", and
        // recording just " B" throws away the "A" that round three will begin with:
        //
        //   round 1 commits "A"        prefix "A"
        //   round 2 commits " B"       prefix "A B"     <- must be the whole thing
        //   round 3 arrives "A B C"    strips "A B", commits " C"
        //
        // With only the last addition kept, every third round matches nothing and the
        // whole accumulation is committed again -- which is exactly the reported fault,
        // and it is why two consecutive rounds looked fine.
        //
        // `text` here is what was drawn, so it is already missing any head that was
        // stripped; appending it to the running prefix reproduces the model's own text.
        let mut prefix = self.last_segment_text.lock().unwrap();
        prefix.push_str(&text);
        self.clear_viewport();
        // Scroll the now-blank slice up, so the strip is empty and the transcript
        // above is untouched.
        let mut out = self.sink();
        let _ = write!(out, "\x1b[{};{}r\x1b[{};1H", top, last, top);
        for _ in 0..shown {
            let _ = write!(out, "\r\n");
        }
        let _ = write!(out, "\x1b[r");
        let _ = out.flush();
        self.committed.store(0, Ordering::Relaxed);
        // A blank line keeps the answer from running into whatever comes next.
        // `insert_history` clears a stream, so calling it here is safe: the answer
        // has already been handed over above.
        self.emit_history("");
    }

    /// Blank the answer strip, so a finished answer is not left on screen twice.
    ///
    /// The strip only: the row below it is the running status line, which belongs to the
    /// clock rather than to the answer. Erasing it here wiped a clock that was still
    /// counting, and the repaint that followed is what made a fast tool flicker through
    /// `── 0s read ──` on its way past.
    fn clear_viewport(&self) {
        let top = self.viewport_top.load(Ordering::Relaxed);
        // The status row is `input_row - 1`, so the strip ends one row above it.
        let last = self.input_row.load(Ordering::Relaxed).saturating_sub(1);
        let mut out = self.sink();
        for row in top..last {
            let _ = write!(out, "\x1b[{};1H\x1b[2K", row);
        }
        let _ = out.flush();
    }

    /// Put the prompt and the current input back on the reserved row.
    pub fn redraw(&self) {
        if !self.interactive {
            return;
        }
        let input = self.input.lock().unwrap();
        let before: String = input.buf[..input.pos].iter().collect();
        let after: String = input.buf[input.pos..].iter().collect();
        drop(input);
        let prefix = self.prefix.lock().unwrap();
        let mut out = self.sink();
        let _ = write!(
            out,
            "\x1b[{};1H\x1b[2K\x1b[1m{}\x1b[0m{before}",
            self.input_row.load(Ordering::Relaxed),
            prefix
        );
        if !after.is_empty() {
            let _ = write!(out, "{after}\x1b[{}D", after.chars().count());
        }
        let _ = out.flush();
    }

    /// Mark the input row as ready to receive, without clearing it.
    pub fn prompt(&self) {
        self.redraw();
    }

    /// Translate a terminal event into a line-editing action.
    ///
    /// Release events are ignored except for Ctrl-C and Ctrl-D: Windows reports
    /// some control keys only as releases, and dropping those makes the key look
    /// dead.
    pub fn on_event(&self, ev: Event) -> Key {
        match ev {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) => {
                let ctrl = modifiers.contains(KeyModifiers::CONTROL);
                let control_key = matches!(code, KeyCode::Char('c') | KeyCode::Char('d'));
                if kind == KeyEventKind::Release && !(ctrl && control_key) {
                    return Key::Ignore;
                }
                let mut input = self.input.lock().unwrap();
                match code {
                    KeyCode::Char('d') if ctrl => Key::Quit,
                    KeyCode::Char('c') if ctrl => {
                        // Text on the line means "clear it"; an empty line means "stop
                        // whatever is running". Neither is quit. Quitting is what a second
                        // Ctrl-C within the window below is for, because losing the
                        // session to a reflex is the expensive mistake here.
                        if !input.is_empty() {
                            input.clear();
                            return Key::Redraw;
                        }
                        let now = std::time::Instant::now();
                        let repeated = self
                            .last_ctrl_c
                            .lock()
                            .unwrap()
                            .map(|then| now.duration_since(then) < std::time::Duration::from_millis(1500))
                            .unwrap_or(false);
                        *self.last_ctrl_c.lock().unwrap() = Some(now);
                        if repeated {
                            Key::Quit
                        } else {
                            Key::Interrupt
                        }
                    }
                    KeyCode::Char(c) => {
                        input.insert(c);
                        Key::Redraw
                    }
                    KeyCode::Backspace => {
                        input.backspace();
                        Key::Redraw
                    }
                    KeyCode::Delete => {
                        input.delete();
                        Key::Redraw
                    }
                    KeyCode::Left => {
                        input.left();
                        Key::Redraw
                    }
                    KeyCode::Right => {
                        input.right();
                        Key::Redraw
                    }
                    KeyCode::Home => {
                        input.home();
                        Key::Redraw
                    }
                    KeyCode::End => {
                        input.end();
                        Key::Redraw
                    }
                    KeyCode::Tab => {
                        input.insert(' ');
                        Key::Redraw
                    }
                    KeyCode::Esc => {
                        input.clear();
                        Key::Redraw
                    }
                    KeyCode::Enter => Key::Enter(input.take()),
                    _ => Key::Ignore,
                }
            }
            Event::Resize(_, _) => {
                // Layout only. `reserve_screen` blanks the display and rewinds the
                // transcript to row 1, so calling it here erased the whole conversation
                // on every resize -- which is also why dragging the window used to look
                // like it "fixed" a stale-glyph artefact: it was clearing the screen.
                //
                // An answer that is still arriving is closed **first**, while the width it
                // was wrapped at is still the width in force. `close_stream` wraps with
                // `screen_cols`, so closing it after the layout moved would hand the
                // transcript rows wrapped for the new window measured against a `committed`
                // count that counts rows of the old one: text the transcript already has
                // would be written again, and the same sentence would sit in the scrollback
                // twice. Closing it here gives the transcript what only the strip had, empties
                // the strip, and lets the next fragment start a fresh segment that is wrapped
                // for the window the reader now has.
                self.close_stream();
                self.reclaim();
                Key::Redraw
            }
            Event::Paste(text) => {
                if !text.contains('\n') && !text.contains('\r') {
                    // A path, a snippet, a word: it belongs in the line being typed.
                    let mut input = self.input.lock().unwrap();
                    for c in text.chars() {
                        input.insert(c);
                    }
                    return Key::Redraw;
                }
                // A block is **one message, with its line breaks kept**. They are part of
                // what was written, and joining them up makes a list, a poem or a stack
                // trace arrive as a single run-on sentence -- which is exactly what a
                // pasted prompt used to look like to the model.
                //
                // Submitted rather than put in the row: the row is one row, so showing a
                // paragraph there would be showing the user something other than what
                // pressing Enter sends.
                Key::Enter(text.trim_end().to_string())
            }
            _ => Key::Ignore,
        }
    }

    /// Give the screen back. Called on every exit path.
    pub fn stop(&self) {
        if !self.interactive {
            return;
        }
        let mut out = self.sink();
        // Drop the scroll region, show the cursor, park below the input row so the
        // shell's next prompt starts on a fresh line.
        // `?2004l` is bracketed paste off, and it has to be off before the shell's next
        // prompt: a terminal left in bracketed paste mode wraps *every* subsequent paste,
        // including into programs that know nothing about it.
        let _ = write!(out, "\x1b[r\x1b[?25h\x1b[?2004l");
        let _ = write!(out, "\x1b[{};1H\r\x1b[2K", self.input_row.load(Ordering::Relaxed));
        let _ = write!(out, "\r\n");
        let _ = out.flush();
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cjk_character_is_two_columns_wide() {
        assert_eq!(display_width("你好"), 4);
        assert_eq!(display_width("磁盘占用正常。"), 14);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("a你b"), 4);
    }

    #[test]
    fn ordinary_punctuation_and_box_drawing_are_one_column() {
        // The `read` tool draws rules with these, and counting them as wide would
        // shift every line of a file by one column per rule.
        assert_eq!(display_width("\u{2502}"), 1);
        assert_eq!(display_width("—"), 1);
        assert_eq!(display_width("…"), 1);
    }

    #[test]
    fn wrapping_counts_columns_and_never_splits_a_wide_character() {
        // A line of Chinese is twice as wide as it has characters, so it must break at
        // half the character count.
        let rows = wrap_rows("你".repeat(60).as_str(), 70);
        assert_eq!(rows.len(), 2, "60 wide characters over 70 columns: {rows:?}");
        assert_eq!(display_width(&rows[0]), 70);
        assert_eq!(display_width(&rows[1]), 50);

        // A wide character that would straddle the boundary moves down whole.
        let rows = wrap_rows("ab你", 3);
        assert_eq!(rows, vec!["ab".to_string(), "你".to_string()]);
    }

    #[test]
    fn wrapping_keeps_explicit_newlines_and_empty_lines() {
        let rows = wrap_rows("a\n\nb", 80);
        assert_eq!(rows, vec!["a".to_string(), String::new(), "b".to_string()]);
    }

    #[test]
    fn a_row_the_screen_width_is_not_wrapped_again() {
        // The boundary case: exactly `cols` columns is one row, not two. Getting this
        // wrong adds a blank row to every full-width line of every answer.
        let rows = wrap_rows(&"x".repeat(80), 80);
        assert_eq!(rows.len(), 1);
    }

    fn press(term: &Term, code: KeyCode) -> Key {
        term.on_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn ctrl(term: &Term, c: char) -> Key {
        term.on_event(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    /// The regression that started this module: Windows reports some control keys
    /// only as Release, so filtering every Release swallowed Ctrl-D and there was
    /// no way to quit.
    #[test]
    fn ctrl_d_is_honoured_on_key_release() {
        let t = Term::plain();
        let ev = Event::Key(KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        });
        assert!(matches!(t.on_event(ev), Key::Quit));
    }

    /// Ordinary characters arriving as Release must still be dropped, or every
    /// keystroke would be typed twice on Windows.
    #[test]
    fn plain_char_release_is_ignored() {
        let t = Term::plain();
        let ev = Event::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        });
        assert!(matches!(t.on_event(ev), Key::Ignore));
        assert!(t.input_mut().is_empty());
    }

    #[test]
    fn ctrl_c_clears_a_draft_then_quits_when_empty() {
        let t = Term::plain();
        press(&t, KeyCode::Char('h'));
        press(&t, KeyCode::Char('i'));
        assert!(matches!(ctrl(&t, 'c'), Key::Redraw));
        assert!(t.input_mut().is_empty());
        // Second press has nothing to clear, so it means quit.
        // One Ctrl-C stops the turn; only a second one within the window quits. The
        // session must not go away because someone pressed the panic key.
        assert!(matches!(ctrl(&t, 'c'), Key::Interrupt));
        assert!(matches!(ctrl(&t, 'c'), Key::Quit));
    }

    #[test]
    fn typing_and_editing_builds_the_line() {
        let t = Term::plain();
        press(&t, KeyCode::Char('a'));
        press(&t, KeyCode::Char('c'));
        press(&t, KeyCode::Left);
        press(&t, KeyCode::Char('b'));
        assert_eq!(t.input_mut().text(), "abc");
        press(&t, KeyCode::Home);
        press(&t, KeyCode::Delete);
        assert_eq!(t.input_mut().text(), "bc");
        press(&t, KeyCode::End);
        press(&t, KeyCode::Backspace);
        assert_eq!(t.input_mut().text(), "b");
    }

    #[test]
    fn enter_hands_over_the_line_and_resets() {
        let t = Term::plain();
        press(&t, KeyCode::Char('h'));
        press(&t, KeyCode::Char('i'));
        assert_eq!(press(&t, KeyCode::Enter), Key::Enter("hi".to_string()));
        // The line must not be submitted twice.
        assert!(t.input_mut().is_empty());
    }

    /// A pasted block is one message, and its line breaks survive.
    ///
    /// This test used to assert the opposite -- that the newlines were stripped -- and it
    /// was testing a path that could not run: bracketed paste was never enabled, so the
    /// terminal never sent the event this handler is for. What actually happened to a
    /// pasted prompt was one message per line, each interrupting the last.
    #[test]
    fn a_pasted_block_is_one_message_with_its_line_breaks() {
        let t = Term::plain();
        match t.on_event(Event::Paste("one\ntwo\r\nthree".to_string())) {
            Key::Enter(text) => assert_eq!(text, "one\ntwo\r\nthree"),
            other => panic!("a block with line breaks in it must be submitted as one message: {other:?}"),
        }
        assert!(
            t.input_mut().is_empty(),
            "a submitted block must not also be left sitting in the input row"
        );
    }

    /// A paste with no line break in it is just text, and goes into the line being typed.
    #[test]
    fn a_paste_without_a_line_break_joins_the_line() {
        let t = Term::plain();
        press(&t, KeyCode::Char('a'));
        assert!(matches!(t.on_event(Event::Paste("/etc/hosts".to_string())), Key::Redraw));
        assert_eq!(t.input_mut().text(), "a/etc/hosts");
    }

    #[test]
    fn esc_abandons_the_line() {
        let t = Term::plain();
        press(&t, KeyCode::Char('n'));
        press(&t, KeyCode::Char('o'));
        assert!(matches!(press(&t, KeyCode::Esc), Key::Redraw));
        assert!(t.input_mut().is_empty());
    }

    /// A non-interactive term must never emit a control sequence: this is the
    /// path `flint exec` and every pipeline take.
    #[test]
    fn plain_term_writes_no_escapes() {
        let t = Term::plain();
        assert!(!t.interactive());
        // `stop` and `prompt` are the two that would otherwise touch the screen.
        t.stop();
        t.prompt();
        t.redraw();
        assert!(!t.interactive());
    }
}
