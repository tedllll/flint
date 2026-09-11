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
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Mutex;
use std::io::{IsTerminal, Write};

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
    /// Ctrl-C on an empty line, Ctrl-D, or EOF: leave the REPL.
    Quit,
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
const ANSWER_ROWS: u16 = 4;
/// The input row plus the answer strip.
const RESERVED: u16 = ANSWER_ROWS + 1;

pub struct Term {
    interactive: bool,
    /// Last row of the screen (1-based, inclusive).
    screen_rows: AtomicU16,
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
            viewport_top: AtomicU16::new(20),
            input_row: AtomicU16::new(24),
            input: Mutex::new(Input::default()),
            prefix: Mutex::new("> ".to_string()),
            stream_active: AtomicBool::new(false),
            stream_text: Mutex::new(String::new()),
            committed: AtomicU16::new(0),
        }
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
        #[cfg(debug_assertions)]
        let interactive = tty || std::env::var_os("FLINT_TERM_CAPTURE").is_some();
        #[cfg(not(debug_assertions))]
        let interactive = tty;

        let term = Term {
            interactive,
            screen_rows: AtomicU16::new(24),
            viewport_top: AtomicU16::new(20),
            input_row: AtomicU16::new(24),
            input: Mutex::new(Input::default()),
            prefix: Mutex::new("> ".to_string()),
            stream_active: AtomicBool::new(false),
            stream_text: Mutex::new(String::new()),
            committed: AtomicU16::new(0),
        };

        if tty {
            // A panic with raw mode on leaves the user without echo, which looks
            // like a broken shell. Restore first, then let the message print.
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let _ = disable_raw_mode();
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b[r\x1b[?25h\r\n");
                let _ = out.flush();
                default_hook(info);
            }));

            enable_raw_mode()?;
            term.reclaim();
            term.setup();
            term.redraw();
        }

        Ok(term)
    }

    pub fn interactive(&self) -> bool {
        self.interactive
    }

    pub fn input_mut(&self) -> std::sync::MutexGuard<'_, Input> {
        self.input.lock().unwrap()
    }

    /// Recompute the layout from the current window size.
    fn reclaim(&self) {
        let (_, h) = size().unwrap_or((80, 24));
        let h = h.max(RESERVED + 1);
        self.screen_rows.store(h, Ordering::Relaxed);
        self.input_row.store(h, Ordering::Relaxed);
        self.viewport_top.store(h - ANSWER_ROWS, Ordering::Relaxed);
    }

    /// Reserve the bottom strip: the answer is drawn there, history above it.
    ///
    /// The whole screen is the scroll region to begin with, because history
    /// insertion sets its own region per write and resets it afterwards.
    fn setup(&self) {
        if !self.interactive {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b[1;{}r\x1b[1;1H", self.screen_rows.load(Ordering::Relaxed));
        let _ = out.flush();
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
    fn insert_history(&self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let bottom = self.history_bottom();
        let mut out = std::io::stdout();
        for text in lines {
            let _ = write!(out, "\x1b[1;{}r", bottom);
            let _ = write!(out, "\x1b[{};1H\r\x1b[2K{text}\r\n", bottom);
            let _ = write!(out, "\x1b[r");
        }
        let _ = write!(out, "\x1b[?25l");
        let _ = out.flush();
        self.redraw();
    }

    /// One line of committed output.
    fn emit_history(&self, text: &str) {
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
        if !self.interactive {
            print!("{text}");
            let _ = std::io::stdout().flush();
            return;
        }
        self.stream_active.store(true, Ordering::Relaxed);
        {
            let mut held = self.stream_text.lock().unwrap();
            // A new answer restarts the line accounting. Within a turn the text only
            // grows, so a shorter text means the previous answer ended and a fresh
            // one began -- reasoning followed by the answer, for instance. Without
            // this the second answer would inherit the first one's committed count
            // and skip lines into history.
            if !text.starts_with(held.as_str()) {
                self.committed.store(0, Ordering::Relaxed);
            }
            *held = text.to_string();
        }

        let rows: Vec<&str> = text.split('\n').collect();
        let height = rows.len() as u16;
        let top = self.viewport_top.load(Ordering::Relaxed);
        let last = self.input_row.load(Ordering::Relaxed).saturating_sub(1);
        let capacity = last.saturating_sub(top).saturating_add(1);

        // Every row of the slice is accounted for, so a line that leaves the top of
        // the slice must be handed to history or it would be lost when the slice
        // scrolls it away.
        let committed = self.committed.load(Ordering::Relaxed);
        if height > capacity {
            let drop = height - capacity;
            if drop > committed {
                let fresh: Vec<String> = rows[committed as usize..drop as usize]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                self.insert_history(&fresh);
                self.committed.store(drop, Ordering::Relaxed);
            }
        }

        let first = height.saturating_sub(capacity) as usize;
        let visible = &rows[first..];
        let start = top + capacity.saturating_sub(visible.len() as u16);
        let mut out = std::io::stdout();
        let last_row = visible.len().saturating_sub(1);
        for (n, line) in visible.iter().enumerate() {
            let r = start + n as u16;
            if r > last {
                break;
            }
            let _ = write!(out, "\x1b[{};1H\x1b[2K{line}", r);
            // Between rows, plain CR+LF puts the cursor at the start of the next one.
            // After the final row, nothing: the cursor is left immediately after the
            // text, which is where the next fragment has to continue from.
            if n < last_row {
                let _ = write!(out, "\r\n");
            }
        }
        let _ = write!(out, "\x1b[?25l");
        let _ = out.flush();
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
        let text = self.stream_text.lock().unwrap().clone();
        if text.is_empty() {
            return;
        }
        let lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        let top = self.viewport_top.load(Ordering::Relaxed);
        let last = self.input_row.load(Ordering::Relaxed).saturating_sub(1);
        let capacity = last.saturating_sub(top).saturating_add(1) as usize;
        let shown = lines.len().min(capacity);
        if shown > 0 {
            let left: Vec<String> = lines[lines.len() - shown..].to_vec();
            self.insert_history(&left);
        }
        self.clear_viewport();
        // Scroll the now-blank slice up, so the strip is empty and the transcript
        // above is untouched.
        let mut out = std::io::stdout();
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
    fn clear_viewport(&self) {
        let top = self.viewport_top.load(Ordering::Relaxed);
        let input = self.input_row.load(Ordering::Relaxed);
        let mut out = std::io::stdout();
        for row in top..input {
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
        let mut out = std::io::stdout();
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
                        if input.is_empty() {
                            Key::Quit
                        } else {
                            input.clear();
                            Key::Redraw
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
                self.reclaim();
                self.setup();
                Key::Redraw
            }
            Event::Paste(s) => {
                let mut input = self.input.lock().unwrap();
                for c in s.chars() {
                    if c != '\r' && c != '\n' {
                        input.insert(c);
                    }
                }
                Key::Redraw
            }
            _ => Key::Ignore,
        }
    }

    /// Give the screen back. Called on every exit path.
    pub fn stop(&self) {
        if !self.interactive {
            return;
        }
        let mut out = std::io::stdout();
        // Drop the scroll region, show the cursor, park below the input row so the
        // shell's next prompt starts on a fresh line.
        let _ = write!(out, "\x1b[r\x1b[?25h");
        let _ = write!(out, "\x1b[{};1H\r\x1b[2K", self.input_row.load(Ordering::Relaxed));
        let _ = write!(out, "\r\n");
        let _ = out.flush();
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A pasted block arrives as one event with newlines in it; those must not be
    /// inserted, or the line breaks the input row.
    #[test]
    fn paste_strips_newlines() {
        let t = Term::plain();
        t.on_event(Event::Paste("one\ntwo\r\nthree".to_string()));
        assert_eq!(t.input_mut().text(), "onetwothree");
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
