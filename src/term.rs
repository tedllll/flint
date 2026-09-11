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
use std::sync::atomic::{AtomicU16, Ordering};
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

/// Rows reserved at the bottom for the input row.
const RESERVED: u16 = 1;

pub struct Term {
    interactive: bool,
    /// Last row of the scrollable output area (1-based, inclusive).
    bottom: AtomicU16,
    /// The reserved input row (1-based).
    input_row: AtomicU16,
    input: Mutex<Input>,
    /// Text before the input, e.g. "> " or "  name: ".
    prefix: Mutex<String>,
}

impl Term {
    /// A terminal that does nothing but print.
    ///
    /// Used for one-shot mode, where the caller wants the answer and nothing
    /// else -- no reserved input row, no raw mode, no escape codes.
    pub fn plain() -> Self {
        Term {
            interactive: false,
            bottom: AtomicU16::new(23),
            input_row: AtomicU16::new(24),
            input: Mutex::new(Input::default()),
            prefix: Mutex::new("> ".to_string()),
        }
    }

    /// Change what is shown before the input. Used by the configuration wizard,
    /// whose questions are not REPL commands.
    pub fn set_prefix(&self, prefix: &str) {
        *self.prefix.lock().unwrap() = prefix.to_string();
    }

    /// Enter raw mode and reserve the bottom row, if there is a console.
    pub fn start() -> Result<Self> {
        let interactive = std::io::stdout().is_terminal();

        let term = Term {
            interactive,
            bottom: AtomicU16::new(23),
            input_row: AtomicU16::new(24),
            input: Mutex::new(Input::default()),
            prefix: Mutex::new("> ".to_string()),
        };

        if interactive {
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

    /// Recompute the reserved row from the current window size.
    fn reclaim(&self) {
        let (_, h) = size().unwrap_or((80, 24));
        let h = h.max(RESERVED + 1);
        self.input_row.store(h, Ordering::Relaxed);
        self.bottom.store(h - RESERVED, Ordering::Relaxed);
    }

    /// Claim the bottom rows: output lives in the region above them.
    fn setup(&self) {
        if !self.interactive {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b[1;{}r\x1b[1;1H", self.bottom.load(Ordering::Relaxed));
        let _ = out.flush();
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
        let mut out = std::io::stdout();
        // Bottom of the scroll region: writing a newline here scrolls the region
        // instead of moving past it, so the reserved row is never touched.
        let _ = write!(out, "\x1b[{};1H\r\x1b[2K", self.bottom.load(Ordering::Relaxed));
        let _ = out.write_fmt(args);
        let _ = write!(out, "\r\n");
        let _ = out.flush();
        self.redraw();
    }

    /// An empty line. `format_args!()` is not a valid format string, so a bare
    /// `println!()` needs its own path.
    pub fn blank(&self) {
        self.line(format_args!(""));
    }

    /// Write text with no trailing newline, for streamed model output where the
    /// caller already holds a complete line.
    pub fn text(&self, args: std::fmt::Arguments<'_>) {        if !self.interactive {
            print!("{args}");
            let _ = std::io::stdout().flush();
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b[{};1H\r\x1b[2K", self.bottom.load(Ordering::Relaxed));
        let _ = out.write_fmt(args);
        let _ = out.flush();
        self.redraw();
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
        let _ = write!(out, "\x1b[r\x1b[?25h");
        let _ = write!(out, "\x1b[{};1H\r\x1b[2K", self.input_row.load(Ordering::Relaxed));
        let _ = write!(out, "\x1b[{};1H\r\n", self.bottom.load(Ordering::Relaxed).max(1));
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
