//! The browser view: one HTML file, and the loopback listener that serves it.
//!
//! `web/view.html` is embedded with `include_str!` rather than written to disk at run time.
//! flint is one binary; a page that could be missing, stale or half-written beside it would
//! be a second thing to get wrong, and the first thing to break on a machine that is already
//! broken. It also means the policy test in `tests/web_view.rs` reads exactly the bytes a
//! listener will serve -- there is no second copy to drift from the one under test.
//!
//! The design, and the reasons for it, are in `docs/web-mode.md`. The short version: the
//! browser is a *window* onto a running flint, not a second mode. The process stays the only
//! writer of the session file, the terminal stays first-class, and closing the tab loses
//! nothing.

/// The whole viewer: HTML, CSS and JS in one file, no build step, no external request.
///
/// Two halves, deliberately kept apart. The first turns lines of either vocabulary -- a
/// session file's messages, or the NDJSON event stream -- into a document; it touches no
/// DOM, which is what lets `scripts/web-view-test.js` run it under Node. The second paints
/// that document, and puts every piece of text in through `textContent`.
pub const VIEW_HTML: &str = include_str!("../web/view.html");

/// The conversation's lines, welded into that page, as one file with no process behind it.
///
/// The whole design is in *what* is welded and *how*, because the page is already the renderer for a
/// finished conversation: dropping a session file on it draws one. An export is that same page with
/// the conversation inside it, so there is exactly one renderer and the export cannot drift from the
/// view -- the alternative, a second reading of a conversation written in Rust, would be a second
/// place where "what a tool call looks like" is decided, and the first place would win every time
/// somebody changed the page.
///
/// Three things are deliberately not what they look like:
///
///   * **The island carries the session's own lines**, not a summary of them: the page's `applyText`
///     already parses `meta`, `chat`, `usage`, `title` and the rest, so an export is the same
///     vocabulary the page has always read. A derived shape here would be derived state in a file
///     that is meant to be opened by somebody else's browser for years.
///   * **`cwd` is removed from the `meta` line.** It is the only field in a session file that names
///     the machine it was written on rather than the conversation, and an export is the one artifact
///     whose whole purpose is to leave that machine. Everything else is kept, including the model and
///     the provider, which are facts about the conversation.
///   * **`<`, `>` and `&` are escaped in the JSON.** The island lives inside a `<script>` element,
///     which ends at the first `</script` in its text -- and the text is a conversation: a tool
///     result quoting a file, or a model asked about this very page. Without the escapes, one of
///     those closes the island early and the rest of the conversation is parsed as HTML, where
///     `<script>` is a script. `\u003c` is the same character to `JSON.parse` and no character at
///     all to the HTML parser, which is the whole trick.
pub fn export_html(lines: &[String], title: &str) -> String {
    let carried: Vec<String> = lines.iter().map(|line| without_cwd(line)).collect();
    let island = serde_json::json!({ "lines": carried }).to_string();
    let island = island
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");

    // The tab names the conversation rather than every export being "a session".
    let page = VIEW_HTML.replace(
        "<title>flint — a session</title>",
        &format!("<title>flint — {}</title>", escaped(title)),
    );
    // The island goes immediately before the page's own script: it has to be in the body for the page
    // to find it by id, and it has to be *before* the script runs, because the boot reads it.
    let script = page
        .find("<script>")
        .expect("the view has one script tag to hang a conversation on");
    format!(
        "{}<script id=\"session\" type=\"application/json\">{island}</script>\n{}",
        &page[..script],
        &page[script..]
    )
}

/// One session line with the directory it was held in taken out of it.
///
/// The lines are carried as the file's own text -- the island is an array of strings, and `applyText`
/// reads them exactly as it reads a dropped file -- so a line is returned untouched unless it is the
/// one line that has a `cwd`. That is not an optimisation: re-serialising every line would rewrite a
/// conversation that has nothing wrong with it, and the export would stop being the file's own words.
/// A line that cannot be parsed is left alone too: the page skips what it does not understand, and
/// rewriting damage would be inventing a record rather than carrying one.
fn without_cwd(line: &str) -> String {
    // A byte-order mark is the encoding's business, not the content's -- the same sentence `attach.rs`
    // has for `@file`, and it matters more here than it looks. `notepad` on Windows writes utf-8 *with*
    // a mark, and so does PowerShell's `Set-Content -Encoding utf8`; JSON stops at the mark, so the
    // `meta` line would be carried as damage, and a `meta` line carried as damage is a line whose
    // `cwd` was never taken out. The export writes a fresh utf-8 document with its own `<meta
    // charset>`, so the mark is not content to carry.
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    if value.get("cwd").is_none() {
        return line.to_string();
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("cwd");
    }
    serde_json::to_string(&value).unwrap_or_else(|_| line.to_string())
}

/// Text for an HTML element: the three characters that mean something to the parser.
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use crate::event::Event;

/// The most of a request head this server will read before giving up on it.
///
/// A browser's request head is under two kilobytes and this is four times that, so the only
/// thing the ceiling can be for is a client that never sends the blank line.
const MAX_HEAD: usize = 8 * 1024;

/// The most of a request *body* this server will read.
///
/// One route has a body -- `POST /message` -- and a message is a thing a person types or
/// pastes, so a megabyte is far more than anyone will send and far less than a client that
/// never stops. The terminal has no such limit, which is deliberate: this one exists because
/// the reader here is a socket that a page can open in a loop.
const MAX_BODY: usize = 1024 * 1024;

/// What the listener knows about the run it belongs to.
///
/// `token` is the credential (§4.2) and `port` is what the `Host` and `Origin` checks
/// compare against (§4.3, §4.4). Both are decided once, when the socket is bound, so no
/// route has to re-derive them and none of them can disagree.
pub struct State {
    token: String,
    port: u16,
    /// The conversation this window shows, when this run is writing one.
    ///
    /// A path and not its contents: `GET /session` re-reads the file every time it is
    /// asked, because the file *is* the record and this process is appending to it while
    /// the browser is looking. A cached body would be derived state, and it would be the
    /// one copy that is wrong -- the session on disk is the only thing the terminal and
    /// the browser can both be right about.
    ///
    /// Behind a lock because the answer changes while the window is open: `/new` and
    /// `/resume` move the run to a different file, and a browser left reading the old one
    /// would be showing a conversation that is not the one in the terminal.
    session: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// The run's event stream, when this run has one to give.
    ///
    /// `None` for a window opened by something that is not a run -- `debug`, a test -- and
    /// for those `/events` says so rather than pretending to be a stream that will never
    /// say anything.
    live: Option<Arc<Live>>,
    /// Where a line typed into the browser goes: the prompt, by way of the caller.
    ///
    /// A `String` and not the REPL's own input enum, and that is the layering rather than an
    /// accident. Everything below this line is a reader of the run and a writer of *text*; the
    /// meaning of a typed line -- that `/` starts a command, that `!` runs a shell command, that
    /// a line arriving mid-turn steers it -- belongs to the REPL, and this file is not a second
    /// place that decides it. `main.rs` adapts this channel into the one the keyboard feeds, so
    /// a message from the page is *the same event* as a message from the keyboard and every
    /// slash command works from the browser because nothing here knows what one is.
    ///
    /// `None` for a run with no prompt to type into -- a one-shot `-p` run -- and for those
    /// `POST /message` says so rather than accepting a message nobody will ever read.
    input: Option<tokio::sync::mpsc::UnboundedSender<FromPage>>,
    /// The directory this run is working in, which is what a **relative** path in a tool result
    /// means.
    ///
    /// A copy of the value rather than a lock, unlike `session`: the working directory is decided
    /// once, before the listener is bound, and nothing in flint changes it afterwards (`--cwd`
    /// chooses it; it does not `chdir`). A run's tools read and write relative to this, so a path
    /// served from some other directory would be a different file with the same name.
    cwd: std::path::PathBuf,
    /// The run's write guard, as `POST /open` reads it.
    ///
    /// A mirror of `Agent::readonly`, and the one piece of this state that is *not* a fact about the
    /// window: it is a fact about the run, which the window has to know because one route here
    /// launches a program and `readonly` is the switch that refuses to launch programs.
    ///
    /// A shared cell rather than a copied `bool`, because `/readonly` can move it while a page is
    /// open -- and it is a mirror rather than the truth, because the truth is the agent's. What
    /// keeps the two from drifting is that every rebuild of the agent passes through one arm of the
    /// REPL, which sets this from the new agent; a stale copy would be a page that launches a
    /// program in a run whose own tools may not.
    readonly: Arc<std::sync::atomic::AtomicBool>,
}

/// How much of a file the page will show.
///
/// The page is a preview, and this is the whole of what makes it one: a session file, a log or a
/// source file is what a person clicks on, and a request that served a 4 GB log would take the run
/// down rather than show the file. Beyond this the answer is cut, in the same bytes, with the cut
/// and the real size in headers -- see `serve_file`.
const FILE_PREVIEW_MAX: usize = 512 * 1024;

/// The size above which a file is not read at all.
///
/// Distinct from the preview cap on purpose: cutting is for a file whose *beginning* is the answer,
/// and this is for one that is not a text file anybody is reading in a browser -- a disk image, a
/// database, a tarball. Reading it to cut it is the part that costs, so it is refused before the
/// first byte is read.
const FILE_REFUSE_ABOVE: u64 = 64 * 1024 * 1024;

/// A line the page sent, and what the page expects to happen to it.
///
/// The text is a command line either way -- the page types `/<name>` and the REPL decides what that
/// means, exactly as it does for the keyboard, and this file still does not know. What is *not* the
/// same is what the terminal should do about the answer:
///
/// - [`FromPage::Line`] is a person typing in the composer. It becomes the same event a keystroke
///   makes, so it steers a turn, prints here, and lands in the transcript, because that is what a
///   typed line does.
/// - [`FromPage::Report`] is the page reading: §8's panel class. It is answered to the page with the
///   terminal quiet, since the terminal is where somebody typed `/help` and the page's reader did
///   not ask for a listing here.
///
/// One channel rather than two, because they are the same queue of work for the same loop, and the
/// distinction is a property of the *request* rather than of where it came from.
///
/// `Debug` and `PartialEq` are for the tests that watch this channel, which is the only way to see
/// what a route did with a body: the value is the whole of the route's effect.
#[derive(Debug, PartialEq, Eq)]
pub enum FromPage {
    Line(String),
    Report(String),
}

/// How many recent frames are kept for a client that reconnects.
///
/// A bound and not a log: this is a transport buffer, not a record. Nothing here is
/// persisted, nothing is derived from it, and losing all of it costs one `reset` and a
/// re-read of the session file -- which is the same thing the page does when it loads.
const RECENT_FRAMES: usize = 512;

/// How often an idle stream says something, even when there is nothing to say.
///
/// A comment, which SSE readers ignore. Middleboxes and browsers drop an idle connection
/// first, and a dropped stream looks exactly like a turn that has not started.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(20);

/// One frame of the event stream: where it sits, and the line it carries.
#[derive(Clone)]
struct Frame {
    seq: u64,
    /// The cursor this frame answers to: the length of the session file at the moment it was
    /// pushed, so that the number a client sends back names a position in the *file* rather
    /// than a position in this process's memory.
    ///
    /// That is the whole difference from `seq`, and it is the difference that survives a
    /// restart: `seq` starts at 1 in every process, so a cursor minted by one process means
    /// nothing in the next one -- which is why every reconnect to a fresh `--web` used to be
    /// answered with `reset`, and the page rebuilt the whole transcript from `/session` to
    /// find out what it had missed. A file position means the same thing to any process
    /// serving that file, and the file is the only thing here that outlives a process.
    ///
    /// A run that keeps **no** session file has no position to name, so its frames carry
    /// `seq` here instead: a frame count, which is exactly as durable as the run it came
    /// from -- and such a run has no file for a cursor to be durable *in*. See `Live::served`.
    bytes: u64,
    /// The SSE event name, for a frame that is about this *stream* rather than about the run.
    ///
    /// Empty for a run event, which is nearly all of them, and those are rendered as a bare
    /// `data:` line so the page applies them with the same vocabulary `--json` writes. A
    /// named frame is the transport talking about itself, and the page handles it before the
    /// run vocabulary is consulted at all.
    name: &'static str,
    line: String,
}

impl Frame {
    fn render(&self) -> String {
        // `id:` is what a reconnecting client sends back as `Last-Event-ID`, so it carries the
        // cursor (`bytes`) and not the process's own numbering. `seq` still orders the ring and
        // is what the server dedupes on; it never leaves the process.
        if self.name.is_empty() {
            format!("id: {}\ndata: {}\n\n", self.bytes, self.line)
        } else {
            format!(
                "id: {}\nevent: {}\ndata: {}\n\n",
                self.bytes, self.name, self.line
            )
        }
    }
}

/// What a new subscriber has to be told before it is up to date.
enum Catch {
    /// The frames it missed, in order. Empty when it is already current.
    Missed(Vec<Frame>),
    /// We cannot prove we still have what it missed, so it should re-read the session.
    Reset,
}

/// The live side of a `--web` run.
///
/// One funnel, two readers. The terminal is fed directly by `run_turn`, and this carries the
/// same events to a browser: the line it pushes is exactly what `flint -p --json` would print
/// for the same event, because it is produced by the same `ndjson::Sink`. That is the whole
/// reason the browser is a fourth renderer rather than a second implementation.
pub struct Live {
    recent: Mutex<VecDeque<Frame>>,
    next: AtomicU64,
    subscribers: broadcast::Sender<Frame>,
    sink: Mutex<crate::ndjson::Sink>,
    /// What the turn is waiting for *now*.
    ///
    /// State rather than history, and kept apart from the ring for that reason: a page that
    /// is opened in the middle of a turn has missed every status frame so far, and the ring
    /// only helps a client that says where it got to. It is sent once on connect and is the
    /// difference between a browser that says what it is waiting for and one that says
    /// nothing at all -- measured, not assumed: the first live capture of a six-second model
    /// call showed a page with no status line.
    status: Mutex<String>,
    /// What the controls on a page could offer, and what they are set to now.
    ///
    /// State rather than history, like `status`, and for the same reason twice over: a page
    /// opened in the middle of a session has missed every earlier frame, and the ring only
    /// helps a client that says where it got to. So it is kept here and sent once on connect.
    ///
    /// It describes the *process* -- which provider and model are in force, what each provider
    /// offers, the settings a person can change and the commands that may be offered -- and not the
    /// conversation, which is why `/new` and `/resume` leave it alone: moving to another
    /// conversation changes nothing that is configured. See [`Live::state`] for why it is a rendered
    /// string rather than a struct.
    state: Mutex<String>,
    /// The session file this stream is about, so that a cursor can be read back out of it.
    ///
    /// The one thing the ring cannot answer from memory: a client whose cursor is older than
    /// everything in the ring -- after a gap of more than [`RECENT_FRAMES`] frames, or in a
    /// process that has only just started -- is answered from the file instead, because the
    /// file still has every entry the ring has dropped. Kept here rather than looked up per
    /// frame because the answer must come from the *same* file the frames were stamped
    /// against; `/new` and `/resume` change it in the same breath as they clear the ring.
    session: Mutex<Option<std::path::PathBuf>>,
}

impl Live {
    pub fn new() -> Arc<Live> {
        // The channel's capacity is per-subscriber buffering, not history: a browser that
        // falls this far behind is told to reset rather than fed a backlog it cannot use.
        let (subscribers, _) = broadcast::channel(RECENT_FRAMES);
        Arc::new(Live {
            recent: Mutex::new(VecDeque::new()),
            next: AtomicU64::new(1),
            subscribers,
            sink: Mutex::new(crate::ndjson::Sink::new()),
            status: Mutex::new(String::new()),
            state: Mutex::new(String::new()),
            session: Mutex::new(None),
        })
    }

    /// Which conversation this stream is about, for a cursor that has to be read out of it.
    ///
    /// Called wherever the run moves to another session file, and once at construction. An
    /// empty `None` is a run that keeps no conversation -- `--no-session` -- and its frames
    /// then carry their own count rather than a file position, because there is no file for a
    /// position to be in.
    pub fn serving(&self, session: Option<std::path::PathBuf>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = session;
    }

    /// The position to stamp the next frame with, when a conversation is being kept at all.
    ///
    /// A `stat`, not a read: this runs once per frame and a frame is a delta of an answer, so
    /// a read here would be the whole conversation re-read hundreds of times per turn. The
    /// file is append-only, so its length only ever moves forward within a conversation, which
    /// is what makes it usable as an ordering at all. `None` is a run that keeps no file
    /// (`--no-session`), which has no position to name and falls back to the frame number.
    fn served(&self) -> Option<u64> {
        let path = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        // A *named* file that is not there yet is position zero rather than no position at all:
        // the file is created by the first thing said in it, and a cursor minted before that has
        // to compare with the ones minted after. Falling back to the frame count here would do
        // the opposite -- a count is a larger number than a short file's length -- so the stamps
        // would go *backwards* the moment the first entry landed.
        Some(std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0))
    }

    /// One event of the run, to every reader of the stream.
    ///
    /// Called from `run_turn`'s event closure -- the one place every event passes -- which
    /// is what makes the browser's stream the same stream `--json` writes rather than a
    /// description of it.
    pub fn event(&self, event: &Event) {
        if let Event::Status { text, .. } = event {
            *self.status.lock().unwrap_or_else(|e| e.into_inner()) = text.clone();
        }
        let line = self
            .sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .line(event);
        if let Some(line) = line {
            self.push(line);
        }
    }

    /// What a command answered, to every reader of the stream.
    ///
    /// §8's other half. A command's answer is not one of the turn's events -- it runs *between*
    /// turns, and the session file deliberately does not hold it -- so it could not be carried by
    /// [`Live::event`], and without it a command typed into the page's composer printed nothing
    /// the page could read. It belongs on the same stream all the same, because the page renders
    /// the transcript from that stream and an action's answer belongs in the transcript.
    ///
    /// An empty answer sends nothing rather than an empty block: `/exit` and a command that only
    /// moves the session say what they have to say another way, and a blank block in the
    /// transcript would read as a rendering fault.
    pub fn command(&self, input: &str, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        self.push(crate::ndjson::command(input, &lines.join("\n")));
    }

    /// The same, for a listing the page asked to read in its own panel.
    ///
    /// §8's report class, and the reason it is a separate call rather than a flag on
    /// [`Live::command`]: the frame carries `panel`, the page puts it in the panel rather than in
    /// the transcript, and a caller that had to remember to say which it was would eventually stop
    /// saying it. An empty answer sends nothing here too -- a report that says nothing is a report
    /// the page should not have asked for.
    pub fn report(&self, input: &str, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        self.push(crate::ndjson::report(input, &lines.join("\n")));
    }

    /// What the turn in flight has written so far, or empty between turns.
    ///
    /// Read out of the sink rather than accumulated separately: it is the same string
    /// `message.completed` drains, so there is one place the answer grows and no way for two
    /// copies of it to disagree.
    fn answer_so_far(&self) -> String {
        self.sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .answer_so_far()
            .to_string()
    }

    /// The answer that was in flight has been committed to the conversation.
    ///
    /// `Event::Done` normally drains the accumulator, and an interrupted turn never reaches it --
    /// the future is dropped, and `Agent::commit_drawn_answer` puts the drawn text into the session
    /// file instead. The feed has to be told the same thing, because `answer_so_far` is what a page
    /// arriving mid-turn is handed as the answer *so far*: left alone, a stopped turn's half would
    /// be read by the next page as an answer still being written, and the turn after it would carry
    /// the stopped text along in its own `message.completed`.
    pub fn answer_committed(&self) {
        self.sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget_answer();
    }

    /// One already-rendered line of the run's vocabulary, to every reader of the stream.
    ///
    /// For a frame that no `Event` produces. `turn.started` is the one, and it is the same line
    /// `--json` writes for the same moment -- taking a rendered line rather than an enum variant
    /// keeps the two writers on the same function, because a second spelling of the vocabulary
    /// here would be a second thing to keep in step.
    pub fn line(&self, line: String) {
        self.push(line);
    }

    /// The numbering of the newest frame the ring still holds, `0` when it holds none.
    ///
    /// Used for one thing: a subscriber that connected a moment before the ring was read can
    /// be handed the same frame twice, and this says which frames were already in the ring.
    fn newest_seq(&self) -> u64 {
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .back()
            .map(|frame| frame.seq)
            .unwrap_or(0)
    }

    fn push(&self, line: String) {
        self.push_named("", line);
    }

    fn push_named(&self, name: &'static str, line: String) {
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        // The cursor: the file as it stands now. Almost every frame is pushed after the entry
        // it is about has been appended, so the position it carries means "everything before
        // here is in the file; ask the file for what comes next". The exceptions are frames
        // about a moment rather than about an entry -- `turn.started` is pushed before the
        // question it opens is written -- and they are harmless in the same direction: their
        // position is a little behind, so a client that stops on one is sent the entries it
        // already has from the file, rather than losing the ones it does not.
        let bytes = self.served().unwrap_or(seq);
        let frame = Frame {
            seq,
            bytes,
            name,
            line,
        };
        {
            let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
            recent.push_back(frame.clone());
            while recent.len() > RECENT_FRAMES {
                recent.pop_front();
            }
        }
        // An error means nobody is listening, which is the normal case.
        let _ = self.subscribers.send(frame);
    }

    /// Tell every reader the list of conversations has changed, as a frame named `sessions`.
    ///
    /// A named frame because the sidebar's data is a *route* (`GET /sessions`) and this says that
    /// route is stale. It is not a fact about the run and it is not the transcript, which is why
    /// it is not `reset`: `reset` makes the page re-read the session file and rebuild the
    /// transcript, and the transcript has not changed.
    ///
    /// Nothing is cleared here either -- the ring, the status and the in-flight answer all belong
    /// to the conversation being *shown*, and `/archive` and `/delete` refuse to touch the open
    /// one for exactly that reason.
    ///
    /// What it exists to prevent is not cosmetic. The numbers in the sidebar are the ones
    /// `/resume` takes, and they are positions in a list: delete one conversation and every number
    /// below it shifts up. A sidebar left showing the old ones sends `/resume 4` for what is now
    /// conversation five, and the person carries on talking in the wrong one.
    pub fn sessions_changed(&self) {
        self.push_named("sessions", String::new());
    }

    /// Tell every reader that the conversation it is showing is not this one any more.
    ///
    /// Called when the run moves to a different session file -- `/new`, `/resume`. The frames
    /// in the ring belong to the conversation being left, so they are **dropped** rather than
    /// replayed: a page that applied them on top of the new file would show two conversations
    /// spliced together, which is worse than showing none. Dropping them also makes every
    /// reconnecting client's cursor too old to satisfy, which is the `reset` it needs; the
    /// named frame below is for the clients that are connected *right now*, which no cursor
    /// difference can reach.
    ///
    /// The status goes with them: it describes a turn *in the conversation that is being left*,
    /// which is the same reason the ring goes. The `state` frame deliberately does not, and the
    /// difference is the point of keeping the two apart: what is configured is a property of the
    /// process, and starting or resuming a conversation changes no provider, model or toggle.
    pub fn restart(&self) {
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = String::new();
        self.push_named("reset", String::new());
    }

    /// A receiver that starts from *now*, with no backlog.
    fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.subscribers.subscribe()
    }

    /// What the turn is waiting for, or empty when it is waiting for nothing.
    fn current_status(&self) -> String {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Tell the page what its controls would show, as a frame named `state`.
    ///
    /// A named frame rather than a line of the run's vocabulary, because it is not an event: it
    /// is where things *are*, like `status`, and it is re-derived from the live configuration
    /// every time the REPL comes back round its loop. That is what makes it honest without being
    /// derived state -- the process is the only writer of its own configuration, and this is a
    /// view of it rather than a second copy to keep in step by hand. The alternative, letting the
    /// page parse `config.toml`, is a second reader that can disagree with the process about a
    /// setting the process is using right now.
    ///
    /// Pushed only when it differs from the last one. The caller runs once per line, so an
    /// unchanged state would otherwise be a frame per keystroke on every open page -- and a
    /// picker rebuilt under the pointer is a control nobody can use. The comparison is on the
    /// rendered JSON, so anything the page can see is a difference that gets through.
    ///
    /// It takes a rendered string rather than a struct so that the vocabulary the page reads is
    /// written in one place, beside the values it comes from (`state_frame` in `main.rs`, which
    /// is what has the config, the agent and the printer).
    pub fn state(&self, json: String) {
        {
            let mut held = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if *held == json {
                return;
            }
            *held = json.clone();
        }
        self.push_named("state", json);
    }

    /// The state as it stands, for a client that has just connected.
    fn current_state(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Everything a client that last saw the file at `at` needs.
    ///
    /// The subscription is taken **before** the recent frames are read, so a frame pushed
    /// in between is delivered by the channel rather than lost in the gap. Losing one there
    /// would be silent, which is the failure this route exists to prevent.
    ///
    /// Two sources, and the order between them is the decision:
    ///
    /// * **The ring**, whenever it still holds the oldest frame at or after the client's
    ///   position. It is the only source with the *deltas* -- the part of an answer streaming
    ///   right now, which is in no file yet -- so a page that dropped for a second mid-answer
    ///   picks up exactly where it was instead of being rebuilt.
    /// * **The file**, whenever it does not. Its entries are sent as `file` frames, which is
    ///   how the page knows these are the record rather than the stream; a gap of more than
    ///   [`RECENT_FRAMES`] frames, and a process that has only just started with a ring of
    ///   nothing, are both answered this way. This is the half that makes a cursor outlive
    ///   the process that minted it.
    ///
    /// `None` -- neither source can prove it has what the client missed, or the client is
    /// showing a different conversation -- is the `reset` it always was.
    fn follow(
        &self,
        last: Option<u64>,
        session: Option<&str>,
    ) -> (broadcast::Receiver<Frame>, Catch) {
        let rx = self.subscribe();
        let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        // No cursor: the client has just loaded, and `/session` is what it read.
        let Some(at) = last else {
            return (rx, Catch::Missed(Vec::new()));
        };
        let covered = match (recent.front(), recent.back()) {
            // The ring holds every frame written at or after the client's position exactly
            // when its oldest frame is behind it and its newest is ahead of it: then nothing
            // the client is missing has been dropped, and nothing it claims to have seen is
            // beyond what we ever sent.
            (Some(oldest), Some(newest)) => oldest.bytes <= at && at <= newest.bytes,
            _ => false,
        };
        if covered {
            let missed = recent.iter().filter(|f| f.bytes > at).cloned().collect();
            return (rx, Catch::Missed(missed));
        }
        match self.entries_after(at, session) {
            Some(missed) => (rx, Catch::Missed(missed)),
            None => (rx, Catch::Reset),
        }
    }

    /// The entries written after `at`, as frames the page applies like a line of the file.
    ///
    /// `None` when the file cannot be the answer: no conversation is being kept, the cursor is
    /// past the end of the one being kept (a cursor from somewhere else, or a file rewritten
    /// under it), the file cannot be read, or the client names a different conversation. That
    /// last check is what the id is for: a position in a file and a position in another file
    /// are the same number, and `/resume` is a run moving between them while a page may be
    /// disconnected and therefore miss the `reset` that would have told it.
    fn entries_after(&self, at: u64, session: Option<&str>) -> Option<Vec<Frame>> {
        let path = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let body = std::fs::read(&path).ok()?;
        if (body.len() as u64) < at || !names_the_same_conversation(&body, session) {
            return None;
        }
        // A position can land inside a line -- a hand edit above the cursor moves every later
        // byte -- so the answer starts at the next line boundary. Half a line is not an entry.
        let mut start = at as usize;
        while start > 0 && start < body.len() && body[start - 1] != b'\n' {
            start += 1;
        }
        let bytes = body.len() as u64;
        // Every frame carries the same cursor, the end of the file: a client that applies all
        // of them has applied the file, and the next frame it is sent is about what comes
        // after. `seq` is zero because these frames were never in the ring and nothing
        // dedupes on them -- the drain in `stream_events` works on the ring's own numbering.
        let missed = body[start..]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| Frame {
                seq: 0,
                bytes,
                name: FILE_FRAME,
                line: String::from_utf8_lossy(line).into_owned(),
            })
            .collect();
        Some(missed)
    }
}

/// The event name a session file's own line is carried under, when the ring cannot answer.
///
/// Named, where every other line of a run is unnamed, because the page treats the two
/// differently: a delta is added to what it has, and a line of the file *is* the record of
/// something it may have drawn from deltas -- so an answer block is filled in from it rather
/// than opened beside it. See `applyEvent`'s `chat` arm in `web/view.html`.
const FILE_FRAME: &str = "file";

/// Whether the bytes of a session file name the conversation the client says it is showing.
///
/// The id is in the `meta` line, which is the first line of every session file, so this reads
/// no further than the first newline. A client that sends no id -- an older page, or a script
/// driving the route by hand -- is not refused: the id is a guard against a *named* mismatch,
/// not a credential.
fn names_the_same_conversation(body: &[u8], session: Option<&str>) -> bool {
    let Some(session) = session.filter(|id| !id.is_empty()) else {
        return true;
    };
    let first = body.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let Ok(line) = serde_json::from_slice::<serde_json::Value>(first) else {
        return true;
    };
    match line.get("id").and_then(|id| id.as_str()) {
        Some(id) => id == session,
        None => true,
    }
}

/// A token for this run, unknown to anything that was not handed it.
///
/// No `rand` dependency. `RandomState` is seeded by the operating system, and hashing a
/// per-process label through two independent instances gives 128 bits that cannot be
/// predicted from outside the process -- which is the strength this needs, because what it
/// defends against is a page in another tab guessing a port and a blind scan of the loopback
/// range, not an offline cryptanalyst. It is deliberately not offered as a general-purpose
/// random source: if flint ever needs one, this is not it.
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut token = String::with_capacity(32);
    for label in ["flint/web/token/1", "flint/web/token/2"] {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write(label.as_bytes());
        hasher.write_u32(std::process::id());
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        token.push_str(&format!("{:016x}", hasher.finish()));
    }
    token
}

// ---------------------------------------------------------------------------
// the request
// ---------------------------------------------------------------------------

/// One HTTP request, as much of it as this server reads.
///
/// A struct rather than a stream of parsing, because every decision this module makes is a
/// question about a *request* -- is the host mine, is the origin mine, is the token right --
/// and those questions are worth being able to ask without a socket. `tests` below drives
/// them with hand-written bytes.
pub struct Request {
    pub method: String,
    /// Path and query, exactly as sent.
    pub target: String,
    /// Lower-cased names, in the order they arrived.
    headers: Vec<(String, String)>,
}

impl Request {
    /// Parse a request head: the request line, the headers, and no body.
    ///
    /// No body, because every route so far is a `GET` and §6's `Connection: close` is what
    /// keeps the corners of HTTP/1.1 out of this file. `POST /message` will add
    /// `Content-Length` framing and nothing else.
    pub fn parse(head: &str) -> Option<Request> {
        let mut lines = head.split("\r\n");
        let mut start = lines.next()?.split(' ');
        let method = start.next()?.to_string();
        let target = start.next()?.to_string();
        if !start.next()?.starts_with("HTTP/1.") {
            return None;
        }
        if !target.starts_with('/') {
            return None;
        }
        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':')?;
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
        Some(Request {
            method,
            target,
            headers,
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The path, without the query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }

    /// One query parameter, undecoded. Enough for a token, which is hex.
    pub fn query(&self, key: &str) -> Option<&str> {
        let query = self.target.split_once('?')?.1;
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    /// The token this request offers, if any.
    ///
    /// The header is accepted everywhere. The query string is accepted **only on `/`**,
    /// because that is the one route a person pastes into a browser and the token has to get
    /// there somehow -- everywhere else it comes from the page's own `fetch`, which can set a
    /// header. A token in a query string can leak through a `Referer`, a log or a shared
    /// link, and the routes that will change something are not the place to accept that.
    fn offered_token(&self) -> Option<&str> {
        match self.header("x-flint-token") {
            Some(token) => Some(token),
            None if self.path() == "/" => self.query("token"),
            None => None,
        }
    }
}

/// Compare two tokens without an early exit.
///
/// Not because a timing attack over a loopback socket is likely, but because the cost is
/// four lines and the alternative is an argument about whether it is likely.
fn token_matches(offered: &str, expected: &str) -> bool {
    let (a, b) = (offered.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for i in 0..a.len() {
        difference |= a[i] ^ b[i];
    }
    difference == 0
}

/// Whether the `Host` header names this listener and nothing else.
///
/// This is the DNS-rebinding defence (§4.3) and it is one string comparison. A hostname an
/// attacker controls can be made to resolve to `127.0.0.1`, which puts
/// `http://evil.example:<port>` in the same *origin* as the listener and defeats the
/// browser's own same-origin protection. The comparison is against the literal address and
/// port that were bound, so a request that arrived through any other name fails here.
fn host_is_ours(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// Whether a `POST` came from our own page.
///
/// Absent is allowed and expected: `curl` sends no `Origin`, and this is a local tool that a
/// person is allowed to drive by hand. Present and foreign is the case that matters -- it is
/// a page the user did not write, asking this program to do something.
fn origin_is_ours(origin: Option<&str>, port: u16) -> bool {
    match origin {
        None | Some("") => true,
        Some(origin) => {
            let origin = origin.to_ascii_lowercase();
            origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}")
        }
    }
}

// ---------------------------------------------------------------------------
// the response
// ---------------------------------------------------------------------------

/// Debug because a `Result<Plan, Response>` has to be unwrapped in a test, and the failure a
/// person wants to read there is the refusal's own sentence rather than a status code.
#[derive(Debug)]
pub struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: String,
    /// Headers a particular route has to add. One so far, and it is a cursor rather than a
    /// security header: see `X-FLINT_EVENT_SEQ`.
    extra: Vec<(String, String)>,
}

/// The event frame the file it accompanies is current as of.
///
/// This is what makes the file and the stream one conversation rather than two views of one
/// that can disagree. A client reads `/session`, notes this number, and then applies only the
/// frames above it: frames at or below it may already be in the file, and frames above it
/// cannot be -- they were emitted after the read. Without it, a client that rendered the file
/// and then connected would either double-count a frame or lose one, and which of the two
/// depended on timing.
const X_FLINT_AT: &str = "X-Flint-At";

impl Response {
    fn text(status: u16, reason: &'static str, body: impl Into<String>) -> Response {
        Response {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
            extra: Vec::new(),
        }
    }

    fn with_header(mut self, name: &str, value: String) -> Response {
        self.extra.push((name.to_string(), value));
        self
    }

    /// A response a program reads rather than a person: `POST /message` and `GET /sessions`
    /// are the two routes with a machine on the other end.
    fn json(status: u16, reason: &'static str, body: String) -> Response {
        Response {
            status,
            reason,
            content_type: "application/json; charset=utf-8",
            body,
            extra: Vec::new(),
        }
    }

    /// One of the four refusals. The reason is short and honest: it is a person debugging
    /// their own URL that reads it, and the listener is on loopback, so there is nothing to
    /// learn from it that a local process could not read anyway.
    fn refused(why: &str) -> Response {
        Response::text(403, "Forbidden", format!("{why}\n"))
    }

    fn render(&self) -> Vec<u8> {
        let mut out = head(self.status, self.reason, self.content_type, self.body.len(), &self.extra);
        out.push_str(&self.body);
        out.into_bytes()
    }
}

/// The status line and the headers of a small, complete response: everything before the body.
///
/// Factored out when the second kind of body arrived (`Answer::Raw`, for a picture). The alternative
/// was a second copy of this `format!`, and the four headers every response carries already proved
/// what a second copy costs: the CSP line was missing from the stream's own copy for as long as it
/// existed. `length` is passed rather than taken from a body, because one of the two callers has no
/// `String` to ask.
fn head(
    status: u16,
    reason: &'static str,
    content_type: &str,
    length: usize,
    extra: &[(String, String)],
) -> String {
    let extra: String = extra
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\
         {SECURITY_HEADERS}{extra}\r\n"
    )
}

/// The four headers every response carries, in one place because there were two.
///
/// They drifted: `SSE_HEADERS` was written by hand beside `Response::render` and its own comment
/// claimed to carry "everything a small response has, except a length", while the CSP line -- the one
/// that makes the page's own inline script the only script it may run, and `connect-src 'self'` the
/// only place it may connect -- was missing from the stream. `docs/features.md` §12.2 promises all
/// four on **every** response, and a promise kept by two literals is kept by neither. The order is
/// what the browser sees rather than what a test expects; `Cache-Control` first, as it always was.
///
/// `img-src` gained `blob:` when the preview learned to draw a picture (§21): the page fetches
/// `/image` with its token in a **header** -- a URL is a token in a history and a log, which is why
/// the query string is accepted on `/` alone -- and hands the bytes to an `<img>` as a blob URL. So
/// the scheme is the page's own, made by the page, and the alternative (`data:`) is already allowed
/// and was refused for a different reason: base64 in a JSON body inflates a photograph by a third
/// and has to be decoded by the page's own script rather than by the browser's decoder.
const SECURITY_HEADERS: &str = "\
Cache-Control: no-store\r\n\
X-Content-Type-Options: nosniff\r\n\
Referrer-Policy: no-referrer\r\n\
Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:\r\n";

/// What a request turned into.
///
/// Three shapes, for three things a response can be: one text response, one response whose body is
/// bytes, and one that has no `Content-Length` at all because it ends when the client goes away.
/// Authorisation and routing still happen in one function either way -- a second place that decided
/// who may connect would be a second place to get §4 wrong.
pub enum Answer {
    /// One response, then close.
    Once(Response),
    /// One response whose body is not text: the image route (`GET /image`).
    ///
    /// A third shape rather than a `Vec<u8>` inside `Response`, because `Response` is text by
    /// construction -- its body is a `String`, every one of its twenty-four construction sites
    /// builds text, and fifty-eight assertions in this file read that text back. Turning the body
    /// into an enum to serve a picture would have touched all of them for one route's sake. What
    /// the two shapes *share* is the header block and [`SECURITY_HEADERS`], which is where the four
    /// headers every response carries actually live, so a third copy of them is still not written.
    Raw(Raw),
    /// The live feed, for a client that last saw `last`.
    Events {
        last: Option<u64>,
        session: Option<String>,
    },
}

/// A response whose body is bytes: what a picture is served as.
pub struct Raw {
    content_type: &'static str,
    body: Vec<u8>,
    extra: Vec<(String, String)>,
}

/// The picture's type and size rather than its bytes.
///
/// `Debug` for `Response`'s reason -- an `expect_err` in a test prints the value it did not expect --
/// and hand-written for one of its own: a derived one would paste several megabytes into a failure
/// message, which is a failure message nobody reads.
impl std::fmt::Debug for Raw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raw")
            .field("content_type", &self.content_type)
            .field("bytes", &self.body.len())
            .finish()
    }
}

impl Raw {
    /// The same header block [`Response::render`] writes, then the bytes.
    fn render(&self) -> Vec<u8> {
        let mut bytes = head(200, "OK", self.content_type, self.body.len(), &self.extra).into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

impl Answer {
    /// The response, for the routes that have exactly one. Panics on a stream, which is a
    /// programming error rather than a client's.
    ///
    /// Test-only: the server itself matches on the shapes, because for one of them
    /// there is nothing to return.
    #[cfg(test)]
    fn once(self) -> Response {
        match self {
            Answer::Once(response) => response,
            Answer::Raw(_) => panic!("this route answers with bytes, not text"),
            Answer::Events { .. } => panic!("this route streams and has no single response"),
        }
    }
}

/// One request, one response -- and every rule of §4 in one place.
///
/// Pure, so the boundary can be tested without a socket and without timing. The order is
/// deliberate: `Host` before anything else, because it is the check that decides whether the
/// browser's same-origin protection applies at all, and a request that fails it is not from
/// a page of ours however good its token looks.
pub fn respond(request: &Request, body: &str, state: &State) -> Answer {
    if !host_is_ours(request.header("host"), state.port) {
        return Answer::Once(Response::refused(
            "this listener answers only to 127.0.0.1 (check the Host header)",
        ));
    }
    if !origin_is_ours(request.header("origin"), state.port) {
        return Answer::Once(Response::refused("this request came from another origin"));
    }
    let allowed = match request.offered_token() {
        Some(offered) => token_matches(offered, &state.token),
        None => false,
    };
    if !allowed {
        return Answer::Once(Response::refused(
            "missing or wrong token (it is in the URL `--web` printed)",
        ));
    }

    Answer::Once(match (request.method.as_str(), request.path()) {
        ("GET", "/") => Response {
            status: 200,
            reason: "OK",
            content_type: "text/html; charset=utf-8",
            body: VIEW_HTML.to_string(),
            extra: Vec::new(),
        },
        // The conversation so far, as the session file's own lines. Not a summary and not a
        // second format: the page is a reader of the same file the terminal is writing, and
        // the moment this route rendered something of its own the two could disagree.
        ("GET", "/session") => serve_session(state),
        // The sidebar. The numbering here *is* the numbering `/resume` accepts, because both
        // go through `session::list` -- a second listing that sorted differently would make
        // every number in the page point at the wrong conversation.
        ("GET", "/sessions") => serve_sessions(state),
        // A file the transcript named, opened where the reader is looking instead of in another
        // program. The one route that reads a path from the wire: see `serve_file`, and §12 of
        // `docs/web-mode.md` for what it will and will not serve.
        ("GET", "/file") => serve_file(request, state),
        // A directory, which is the one thing `/file` refuses by its own rule and the one a person
        // pressing a path in a transcript most often wants: the argument of a `list`, the directory a
        // `glob` walked, a path the model wrote. It answers as *data* rather than as text -- the panel
        // draws each entry as a control, and an entry whose name has a space in it is a name rather
        // than two tokens -- and it is the same listing the `list` tool prints, from the same
        // function: see `serve_dir` and §27 of `docs/web-mode.md`.
        ("GET", "/dir") => serve_dir(request, state),
        ("GET", "/resolve") => serve_resolve(request, state),
        // The other half of the same question: a path the panel cannot draw as text. A picture is
        // not a file too big or too strange to preview, it is a file this page can show *only* if
        // it is handed the bytes with the type they really are -- see `serve_image`, and §21 of
        // `docs/web-mode.md`.
        //
        // Returned rather than wrapped, because its body is not a `String`: it is the one route
        // besides `/events` that answers with another shape.
        ("GET", "/image") => return serve_image(request, state),
        // The other half of that answer: the same path handed to the program this machine uses for
        // it, for the files and directories the panel above cannot show -- a directory, a file too
        // large to preview, anything that is not text. The second route in this file that can make
        // something *happen*, and the first that starts a process: see `serve_open`, and §16 of
        // `docs/web-mode.md`.
        ("POST", "/open") => serve_open(body, state),
        // The run's own background work: the `task` children and the background commands this
        // process started, and the only place a person sees them without asking the model. Read off
        // the same `Job` records `job_op` answers from, so the page and the tool cannot describe one
        // job two ways -- §13 of `docs/web-mode.md` is the record.
        ("GET", "/jobs") => {
            Response::json(200, "OK", crate::tools::jobs_snapshot().to_string())
        }
        // Who else is working in this directory, which is what `/say --to` addresses. The fourth
        // route of this shape and the first that is about the *machine* rather than about this run:
        // a page holds its own jobs and conversations, and other runs are a fact it cannot derive.
        // Read on request rather than pushed on a clock -- the page asks when the picker is drawn,
        // which is the only moment the list is wanted -- and answered from `live::peers_here`, the
        // same list the terminal's `/say` addresses and describes, so the two cannot disagree about
        // who is in the room. See `docs/web-mode.md` §8.
        ("GET", "/peers") => Response::json(200, "OK", crate::live::peers_snapshot(&state.cwd).to_string()),
        // Everything since, and then everything as it happens.
        ("GET", "/events") => return Answer::Events {
            last: last_event_id(request),
            session: session_of(request),
        },
        // A line typed into the browser, into the same channel the keyboard feeds.
        ("POST", "/message") => accept_line(body, state, true),
        // The same channel, and a different intent: the page is asking for a listing to read in its
        // own panel. `false` here is the whole difference -- what the line means is still the REPL's
        // business, and whether the terminal should print the answer is not.
        ("POST", "/report") => accept_line(body, state, false),
        // The page's own account of itself, written where it can be read afterwards. See
        // `accept_log` for why the browser's console was not good enough.
        ("POST", "/log") => accept_log(body.as_bytes()),
        // Before the catch-alls, or `POST /` would be reported as a missing route rather
        // than as a route that exists and takes no body.
        (_, "/") => Response::text(
            405,
            "Method Not Allowed",
            "the view is served, not written to\n",
        ),
        ("GET", _) | ("HEAD", _) | ("POST", _) => {
            Response::text(404, "Not Found", "no such route\n")
        }
        _ => Response::text(404, "Not Found", "no such route\n"),
    })
}

/// The conversations `/resume` can reach, numbered the way `/resume` numbers them.
///
/// Read fresh on every request, like `/session`: the list changes as conversations are made,
/// named and filed away, and a cached copy would be wrong exactly while somebody is looking at
/// it. `session::list` reads only the two ends of each file, so listing is cheap whatever the
/// conversations weigh.
/// Where the page's own account of itself goes: `<FLINT_HOME>/web.log`, one plain line per entry.
///
/// The browser's console is not a place a report can be read from. It belongs to whoever is
/// sitting at that machine, and "the page does not change" was reported four times over it without
/// a single line of it reaching the person who could act -- while from the outside three different
/// failures (no frames, frames skipped, frames rendered out of sight) look exactly the same. A
/// file can be read afterwards, by whoever is looking, from wherever they are, and this repository
/// already keeps its state as plain files for the same reason.
///
/// It sits beside the sessions because that is `FLINT_HOME` and nothing here needs a second notion
/// of where flint keeps things. Capped, because a page left open overnight must not be able to
/// fill a disk; single-lined, because a log in which one entry can look like several is a log that
/// lies; and it timestamps entries, because "which of these happened before the restart" is the
/// only question a log like this is ever asked.
fn accept_log(body: &[u8]) -> Response {
    accept_log_in(&crate::config::sessions_dir().with_file_name("web.log"), body)
}

/// The same, with the file given, so a test does not have to touch a real `FLINT_HOME`.
fn accept_log_in(path: &std::path::Path, body: &[u8]) -> Response {
    const CAP: u64 = 1024 * 1024;
    let line: String = String::from_utf8_lossy(body)
        // Collapsed rather than only replaced: one entry is one line, and a page that ends its
        // entry with a newline must not leave a trailing space that reads as a stray field.
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect();
    if line.trim().is_empty() {
        return Response::text(400, "Bad Request", "an empty log line is not a record\n");
    }
    let written = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if written > CAP {
        return Response::text(200, "OK", "the log is full\n");
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = format!("{stamp} {line}\n");
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, entry.as_bytes()));
    match appended {
        Ok(()) => Response::text(200, "OK", "logged\n"),
        Err(e) => Response::text(
            500,
            "Internal Server Error",
            format!("cannot write the log: {e}\n"),
        ),
    }
}

fn serve_sessions(state: &State) -> Response {
    serve_sessions_in(&crate::config::sessions_dir(), state)
}

/// The same, with the directory given.
///
/// Split out so a test can point it at a directory of its own. The alternative -- setting
/// `FLINT_HOME` for the duration -- would change the environment under every other test running
/// in the same process, which is a race that shows up as a mystery failure elsewhere.
fn serve_sessions_in(dir: &std::path::Path, state: &State) -> Response {
    let open = state
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().to_string());
    let listed = match crate::session::list(dir) {
        Ok(listed) => listed,
        Err(e) => {
            return Response::text(500, "Internal Server Error", format!("{e:#}\n"));
        }
    };
    let sessions: Vec<serde_json::Value> = listed
        .iter()
        .enumerate()
        .map(|(index, (id, label))| {
            serde_json::json!({
                "n": index + 1,
                "id": id,
                // The label `session::list` chose: the newest name given to the conversation,
                // else the first thing that was said, else "(empty)". Chosen in one place so
                // the sidebar and `/sessions` cannot describe the same file differently.
                "label": label,
                "current": Some(id.as_str()) == open.as_deref(),
            })
        })
        .collect();
    Response::json(
        200,
        "OK",
        serde_json::json!({ "sessions": sessions }).to_string(),
    )
}

/// One line from the page, into the prompt: typed, or asked for as a report.
///
/// This is the route that makes the page a composer, and it is the only kind of route in this
/// file that can cause something to *happen* -- the others read. What keeps that acceptable is
/// unchanged from §4: loopback only, the `Host` and `Origin` checks, and a token that the page has
/// and another origin's page does not. A cross-origin form post cannot set `X-Flint-Token`, and a
/// `fetch` that could would be refused by the `Origin` check before it got here.
///
/// Note what it does *not* do: it does not interpret the text. A line beginning `/` is a slash
/// command because the REPL says so, a line beginning `!` is a shell escape because the REPL
/// says so, and neither fact is known in this file. That is the point -- there is one place
/// that decides what a typed line means, and the browser is not a second one. It follows that this
/// route does not check *which* command a report names: the table that says a command is a report
/// lives beside the dispatch, and the REPL refuses anything else. A whitelist here would be a second
/// copy of that table, in the one file that is proudest of not having one.
///
/// `typed` is the only difference between the two routes: a typed line is a keystroke and a report
/// is a request to read, and the REPL is told which it is so that it knows whether to print.
fn accept_line(body: &str, state: &State, typed: bool) -> Response {
    let Some(input) = &state.input else {
        return Response::text(
            409,
            "Conflict",
            "this run has no prompt to type into (a one-shot run reads no input)\n",
        );
    };
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(e) => {
            return Response::text(400, "Bad Request", format!("expected a JSON body: {e}\n"));
        }
    };
    let Some(text) = parsed.get("text").and_then(|t| t.as_str()) else {
        return Response::text(400, "Bad Request", "expected {\"text\": \"...\"}\n");
    };
    if text.trim().is_empty() {
        return Response::text(400, "Bad Request", "an empty message is not one\n");
    }
    let sent = if typed {
        input.send(FromPage::Line(text.to_string()))
    } else {
        input.send(FromPage::Report(text.to_string()))
    };
    match sent {
        Ok(()) => Response::json(202, "Accepted", "{\"queued\":true}".to_string()),
        // The receiver is gone, so the run is ending. Saying so is better than a 202 for a
        // message that will never be read: a page that shows what it sent would otherwise
        // show a message the terminal never saw.
        Err(_) => Response::text(409, "Conflict", "this flint is shutting down\n"),
    }
}

/// Which frame the client last saw, from the header SSE defines for it.
///
/// A query parameter is accepted as well, and for a mundane reason: the page reads this
/// stream with `fetch` rather than with `EventSource`, because `EventSource` cannot send the
/// token header that §4.2 requires -- and a hand-written client should not have to gamble on
/// which headers a browser lets it set. It is a cursor, not a credential, so putting it in
/// the URL costs nothing.
fn last_event_id(request: &Request) -> Option<u64> {
    request
        .header("last-event-id")
        .or_else(|| request.query("last"))
        .and_then(|value| value.trim().parse().ok())
}

/// The conversation the client believes it is showing, if it said.
///
/// Sent beside the cursor because a cursor is a position in a file and two files have the same
/// positions: it is what lets the server refuse to answer a client that is showing one
/// conversation with the entries of another. See [`Live::entries_after`].
fn session_of(request: &Request) -> Option<String> {
    request.query("session").map(str::to_string)
}

/// The session file, exactly as it is on disk.
///
/// Read every time, synchronously, and the reasoning is worth stating because both halves
/// look like mistakes. *Read every time* because nothing here is derived: the file is the
/// record, this process appends to it, and a cached body would go stale exactly while
/// someone is watching a turn. *Synchronously* because it is one local file of a few
/// kilobytes, and `tools::walk_files` already reads directories the same way from inside the
/// async tool loop -- a background read would be a task and a channel to avoid a millisecond.
///
/// Served **as it is**, including a last line with no newline after it. That case is a read
/// that caught the file mid-append, and the tempting fix -- drop the unfinished line -- is
/// wrong: `session::load` reads such a line, so dropping it here would make the browser show
/// less than the terminal does. A torn read is transient; the next refresh has it whole.
fn serve_session(state: &State) -> Response {
    // Cloned out so the lock is not held across the file read: `restart` wants it while a
    // `/new` is switching sessions, and a slow read of a large session file is exactly when
    // it would want it.
    let path = state
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(path) = path else {
        return Response::text(
            404,
            "Not Found",
            "this run is not writing a session, so there is nothing to show\n",
        );
    };
    match std::fs::read_to_string(&path) {
        Ok(body) => {
            // The cursor this body *is*: the position in the file the client has just read to
            // its end. Read off the body rather than `stat`ed again, so the two cannot
            // disagree by a byte appended in between -- the client would then apply a line it
            // already has.
            let mut response = Response {
                status: 200,
                reason: "OK",
                content_type: "application/x-ndjson; charset=utf-8",
                body,
                extra: Vec::new(),
            };
            if state.live.is_some() {
                let at = response.body.len().to_string();
                response = response.with_header(X_FLINT_AT, at);
            }
            response
        }
        // The file is created by the first thing said in it, and the run has a session named
        // before that -- so a fresh run's page asks for a path that is not there yet, and it is
        // asking about a conversation that is genuinely empty rather than about a fault. Serving
        // that as a 500 (which it was, measured in a browser) left the page unable to draw any of
        // its controls, because it cannot finish until this route has answered. An empty body is
        // what an empty file would have been, and the page reads the file or nothing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Response {
            status: 200,
            reason: "OK",
            content_type: "application/x-ndjson; charset=utf-8",
            body: String::new(),
            extra: Vec::new(),
        },
        Err(e) => Response::text(
            500,
            "Internal Server Error",
            format!("cannot read {}: {e}\n", path.display()),
        ),
    }
}

/// The line a `:N` or `:N:C` suffix named, as the header that carries it.
const X_FLINT_LINE: &str = "X-Flint-Line";

/// How many bytes of the file were served, and how many there are, when it was cut.
const X_FLINT_CUT: &str = "X-Flint-Cut";
const X_FLINT_SIZE: &str = "X-Flint-Size";

/// One file, opened where the reader is already looking: `GET /file?path=…`.
///
/// Why this route exists: flint's own tools report paths -- `read`, `write`, `edit`, `grep` and
/// `glob` all name files -- and until this route there was no way to look at one without leaving
/// the page for another program. DSH's page solves the same problem by opening a file reference in
/// a document preview beside the conversation; this copies the idea and none of the machinery: no
/// file tree, no renderer registry, no PDF support, one route and one panel. What the panel draws is
/// text here, a picture through [`serve_image`], and a Markdown document through the page's own
/// renderer (§20 of `docs/web-mode.md`) -- three answers to one press, each of them one route or one
/// function rather than a plugin surface.
///
/// **What it may serve, and why that is not a hole.** The token is the only credential, exactly as
/// for every route, and whoever holds it can already `POST /message` -- that is, type a line into a
/// run with no permission layer and no approval prompts, which can read or write any file this
/// process can. So this route hands the holder of the token nothing they did not have. Pretending
/// otherwise, by serving only paths under the working directory, would be security theatre in a
/// program whose whole documented position is that it can damage the machine; what this route
/// *does* refuse is what has no honest answer -- a directory, a file that is not text, a file too
/// large to be a preview.
///
/// A path is a **parameter**: `/file?path=…`, never `/file/…`, so the route table stays literal and
/// there is no traversal in a route (§6). A relative path is resolved against the run's working
/// directory, because that is what a relative path in a tool result means; an absolute one is used
/// as it is. A path with a `:line` or `:line:column` on the end -- which is how `grep` prints a hit
/// -- falls back to the file before the numbers, and the line comes back in a header so the page
/// can open where the hit was found. The literal path is tried **first**, so a Windows drive letter
/// or a name that really has a colon in it is never mistaken for a line number.
fn serve_file(request: &Request, state: &State) -> Response {
    let Some(raw) = request.query("path") else {
        return Response::text(
            400,
            "Bad Request",
            "which file? this route takes ?path=<path> (and ?path=<path>:<line> opens at a line)\n",
        );
    };
    let Some(asked) = percent_decode(raw) else {
        return Response::text(400, "Bad Request", "that path is not percent-encoded properly\n");
    };
    if asked.trim().is_empty() {
        return Response::text(400, "Bad Request", "which file? ?path= was empty\n");
    }

    // The literal path first, then the same path without a trailing `:N`/`:N:C`. Both go through
    // the same read, so there is one set of refusals rather than two that can disagree -- and a
    // Windows drive letter, or a name that really has a colon in it, is never mistaken for a line
    // number, because the literal path gets its chance first.
    let literal = resolve(&asked, &state.cwd);
    if let Ok(preview) = read_preview(&literal) {
        return preview.into_response();
    }
    if let Some((path, line)) = split_line(&literal) {
        if let Ok(mut preview) = read_preview(&path) {
            preview.headers.push((X_FLINT_LINE.to_string(), line.to_string()));
            return preview.into_response();
        }
    }
    refusal(&asked, &literal)
}

/// What a file read produced: its text, and the headers that describe the read.
struct Preview {
    text: String,
    headers: Vec<(String, String)>,
}

impl Preview {
    fn into_response(self) -> Response {
        Response {
            status: 200,
            reason: "OK",
            content_type: "text/plain; charset=utf-8",
            body: self.text,
            extra: self.headers,
        }
    }
}

/// Read at most [`FILE_PREVIEW_MAX`] of a file, cutting on a character boundary and saying so.
///
/// One `Err(())` for every way this can fail to be a file worth showing. The caller turns it into
/// the sentence that names what the path actually is, because that is a question about the path
/// rather than about the read.
fn read_preview(path: &std::path::Path) -> Result<Preview, ()> {
    let metadata = std::fs::metadata(path).map_err(|_| ())?;
    if metadata.is_dir() || metadata.len() > FILE_REFUSE_ABOVE {
        return Err(());
    }
    let mut file = std::fs::File::open(path).map_err(|_| ())?;
    // Bounded by the cap rather than by the file: the extra byte beyond the cap is what tells this
    // read that there is more, and reading the rest of a 40 MB log to find out is the cost the cap
    // exists to avoid.
    let mut bytes = Vec::new();
    use std::io::Read as _;
    std::io::Read::take(&mut file, FILE_PREVIEW_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let cut = bytes.len() > FILE_PREVIEW_MAX;
    let mut headers = Vec::new();
    let mut text = as_text(bytes).ok_or(())?;
    if cut {
        // On a boundary, so the page never renders half a character: `as_text` gives the whole
        // prefix when it *is* whole, and the two disagree only when the cut fell inside one.
        let end = text
            .char_indices()
            .take_while(|(at, _)| *at < FILE_PREVIEW_MAX)
            .last()
            .map(|(at, c)| at + c.len_utf8())
            .unwrap_or(0);
        text.truncate(end);
        headers.push((X_FLINT_CUT.to_string(), text.len().to_string()));
        headers.push((X_FLINT_SIZE.to_string(), metadata.len().to_string()));
    }
    Ok(Preview { text, headers })
}

/// Bytes as text, shrinking a truncated character at the end and refusing anything else.
///
/// `String::from_utf8_lossy` is deliberately not used: it would turn a binary file into a field of
/// replacement characters and call that a preview. The difference between "this is not text" and
/// "the cap landed inside a character" is `error_len`, which is `None` only for the second.
fn as_text(mut bytes: Vec<u8>) -> Option<String> {
    loop {
        match String::from_utf8(bytes) {
            Ok(text) => return Some(text),
            Err(e) => {
                let error = e.utf8_error();
                let whole = error.valid_up_to();
                if error.error_len().is_some() || whole == 0 {
                    return None;
                }
                let mut raw = e.into_bytes();
                raw.truncate(whole);
                bytes = raw;
            }
        }
    }
}

/// The `path:line` in a path that is not there, when there is one.
fn split_line(path: &std::path::Path) -> Option<(std::path::PathBuf, u32)> {
    let text = path.to_string_lossy().to_string();
    let (rest, tail) = text.rsplit_once(':')?;
    let last: u32 = tail.parse().ok()?;
    // `notes.txt:3:1`, which is how a grep with column numbers prints a hit: the line comes first
    // and the column last, so the number *before* the final one is the line. The column itself is
    // dropped, because a preview opens at a line and a character offset inside it was not asked
    // for. A Windows path survives this: the segment before the last colon is `C` or a directory,
    // and neither parses as a number.
    if let Some((head, before)) = rest.rsplit_once(':') {
        if !head.is_empty() {
            if let Ok(line) = before.parse::<u32>() {
                return Some((std::path::PathBuf::from(head), line));
            }
        }
    }
    if rest.is_empty() {
        return None;
    }
    Some((std::path::PathBuf::from(rest), last))
}

/// The refusal for a path that could not be read, said in words that name what it actually is.
fn refusal(asked: &str, path: &std::path::Path) -> Response {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Response::text(
            400,
            "Bad Request",
            format!("{asked} is a directory, not a file\n"),
        )
        // ...and the *kind* of thing it is in a header as well, because the page acts on it: a
        // directory is read through `GET /dir`, and a page that had to read this sentence to find
        // that out would be parsing prose -- prose this process wrote, which is worse than parsing a
        // stranger's, because it looks safe.
        .with_header("X-Flint-Dir", "1".to_string()),
        Ok(metadata) if metadata.len() > FILE_REFUSE_ABOVE => Response::text(
            400,
            "Bad Request",
            format!(
                "{asked} is {} -- too big to preview; open it where it lives\n",
                bytes_label(metadata.len())
            ),
        ),
        Ok(_) => Response::text(
            400,
            "Bad Request",
            format!("{asked} is not text this page can show (it is not valid UTF-8)\n"),
        ),
        Err(e) => Response::text(404, "Not Found", format!("nothing at {asked}: {e}\n")),
    }
}

/// The most entries one listing will describe, and the reason a listing can be bounded at all.
///
/// A directory can hold a hundred thousand names, and every entry here is drawn as a control: the cap
/// is what keeps one press from building a page nobody can use. What is there is still *counted*, so a
/// reader is told the directory holds more than what they can see rather than being shown a quietly
/// shortened list -- the same rule `GET /file` follows when it cuts a long file.
const DIR_ENTRIES_MAX: usize = 2000;

/// `GET /dir?path=…`: what is in a directory, as the data the panel draws.
///
/// The answer to a press that used to be a refusal. A directory path in a transcript is a button --
/// the argument of a `list`, the directory a `glob` walked, something a model wrote -- and pressing one
/// used to fill the panel with "is a directory, not a file", which is true and useless.
///
/// It answers as JSON because the page builds a *control* per entry (one press to go in, one press to
/// read a file) and because an entry's name is one name: `My Projects` is a single entry here and two
/// tokens to anything reading the transcript as text, which is the whole reason a directory with a
/// space in its name could never be pressed before. Each entry carries the **full path**, joined by
/// this process -- separators are the process's business, and a page joining a name onto a directory
/// would be a second opinion about paths on a machine it cannot see.
///
/// The listing itself comes from `tools::directory_items`, the same function the `list` tool prints
/// from, so a model and a person looking at one directory are told the same thing in the same order.
fn serve_dir(request: &Request, state: &State) -> Response {
    let Some(raw) = request.query("path") else {
        return Response::text(
            400,
            "Bad Request",
            "which directory? this route takes ?path=<directory>\n",
        );
    };
    let Some(asked) = percent_decode(raw) else {
        return Response::text(400, "Bad Request", "that path is not percent-encoded properly\n");
    };
    if asked.trim().is_empty() {
        return Response::text(400, "Bad Request", "which directory? ?path= was empty\n");
    }
    let literal = resolve(&asked, &state.cwd);
    match std::fs::metadata(&literal) {
        Err(e) => Response::text(404, "Not Found", format!("nothing at {asked}: {e}\n")),
        Ok(metadata) if !metadata.is_dir() => Response::text(
            400,
            "Bad Request",
            format!("{asked} is a file, not a directory\n"),
        ),
        Ok(_) => match crate::tools::directory_items(&literal) {
            Err(e) => Response::text(400, "Bad Request", format!("cannot list {asked}: {e}\n")),
            Ok(items) => {
                let total = items.len();
                let entries: Vec<serde_json::Value> = items
                    .iter()
                    .take(DIR_ENTRIES_MAX)
                    .map(|item| {
                        serde_json::json!({
                            // The name, the line the `list` tool would print for it, the path this
                            // process joined, and the two facts the panel styles a row with.
                            "name": item.name,
                            "line": item.line(),
                            "path": literal.join(&item.name).to_string_lossy(),
                            "dir": item.dir,
                            "size": item.size,
                        })
                    })
                    .collect();
                Response::json(
                    200,
                    "OK",
                    serde_json::json!({
                        "path": literal.to_string_lossy(),
                        "parent": literal.parent().map(|p| p.to_string_lossy().to_string()),
                        "entries": entries,
                        "total": total,
                        "shown": entries.len(),
                    })
                    .to_string(),
                )
            }
        },
    }
}

/// `GET /resolve?text=…`: where, in a line of text, does the path end?
///
/// The one question a *reader* of the transcript cannot answer and this process can. A bare path with
/// a space in it is two tokens to any scanner -- `C:\Users\me\My Documents` is one name and two words,
/// and no rule about the text can tell it from a path followed by a word -- so the page asks the run,
/// which is the only thing here that has a filesystem. Reported directly, 2026-09-23, on that exact
/// path: *a directory with a space in it is not recognised at all.*
///
/// The answer is the **longest prefix that exists**, and how many characters of the text it used. The
/// page cuts the line there and draws the button, so what a person reads as the link is exactly what
/// the run found -- and the words that were tried and rejected stay plain text.
///
/// A reading, like `/file`: it stats paths and serves nothing. It answers with `used` because the
/// answer's use is a `slice` in a browser, and with `path` so a caller -- and a test -- can see what
/// was actually found rather than only how long it was.
///
/// **The text begins at the path.** That is the whole contract, and it is the page's half of the
/// question: `C:\work\x` in a sentence is found by the scanner as a candidate, and what it sends is
/// the line *from that candidate's first character* on. A caller that handed over a whole sentence
/// would be asking this route to find where the path *starts*, which is the part the scanner does.
fn serve_resolve(request: &Request, state: &State) -> Response {
    let Some(raw) = request.query("text") else {
        return Response::text(400, "Bad Request", "which text? this route takes ?text=<some text>\n");
    };
    let Some(text) = percent_decode(raw) else {
        return Response::text(400, "Bad Request", "that text is not percent-encoded properly\n");
    };
    if text.trim().is_empty() {
        return Response::text(400, "Bad Request", "which text? ?text= was empty\n");
    }
    match path_in_text(&text, &state.cwd) {
        Some((path, used)) => Response::json(
            200,
            "OK",
            serde_json::json!({ "path": path, "used": used }).to_string(),
        ),
        None => Response::text(
            404,
            "Not Found",
            format!(
                "nothing in {} names a path this run can reach\n",
                crate::util::truncate(&text, 120)
            ),
        ),
    }
}

/// The longest prefix of `text` that names something on this machine, and how much of it that was.
///
/// Word by word, longest first, with trailing punctuation trimmed at each step, so `read
/// <path>\My Notes.txt, and then` names the file rather than the comma -- and a sentence that begins
/// after a path is not swallowed by it, which is the half a page cannot get right on its own: it can
/// only guess where a name ends, and this can look.
///
/// Two fallbacks, both of them the rules `/file` already keeps: a trailing `:N`/`:N:C` is tried as a
/// line number after the literal path has had its chance (a Windows drive letter or a name that
/// really has a colon in it is never mistaken for one), and every candidate goes through
/// [`resolve`], so a relative name is the run's own and `~/x` is the home directory.
///
/// `used` counts **UTF-16 code units**, because the reader is a JavaScript string: `slice(0, used)`
/// is how the page cuts the line. That is asserted with a name in Chinese rather than assumed.
fn path_in_text(text: &str, cwd: &std::path::Path) -> Option<(String, usize)> {
    let units = |s: &str| -> usize { s.chars().map(char::len_utf16).sum() };
    // Every place a word could end, longest first: the whole text, then just before each space.
    let mut ends: Vec<usize> = Vec::new();
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            ends.push(i);
        }
    }
    ends.push(text.len());
    ends.reverse();

    for end in ends {
        let candidate = text[..end].trim_end_matches(TRAILING_PUNCTUATION);
        if candidate.is_empty() {
            continue;
        }
        let literal = resolve(candidate, cwd);
        if std::fs::metadata(&literal).is_ok() {
            return Some((literal.to_string_lossy().to_string(), units(candidate)));
        }
        if let Some((path, _line)) = split_line(&literal) {
            if std::fs::metadata(&path).is_ok() {
                // The line number stays *inside* what was used: the button on the page reads
                // `notes file.txt:12`, because that is what the line says, and the press is what
                // turns it into the path and the line.
                return Some((path.to_string_lossy().to_string(), units(candidate)));
            }
        }
    }
    None
}

/// What is punctuation rather than part of a name when a candidate is tried.
///
/// The sentence's, not the name's: `read <path>, and then` has a comma after the file, and a name
/// that really ends in one is a name this resolver will not find -- which is the honest trade, since
/// the alternative is to hand `/file` a path that does not exist.
///
/// The four full-width marks (U+3002 the ideographic full stop, U+FF0C the full-width comma, U+3001 the
/// enumeration comma, U+FF1A the full-width colon) are escapes rather than characters, and that is the
/// mojibake guard's rule rather than a preference: a Rust source in this tree holds no CJK, so damage in
/// one stays visible. They are here because a model writing Chinese between a path and the rest of its
/// sentence uses them, and a resolver that only knew the ASCII punctuation would glue one onto the name
/// it had found.
const TRAILING_PUNCTUATION: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\'', '>', '\u{3002}', '\u{ff0c}', '\u{3001}',
    '\u{ff1a}',
];

/// The biggest picture this page will draw.
///
/// Smaller than [`FILE_REFUSE_ABOVE`] on purpose, and the difference is not tidiness: a text file is
/// served as at most 512 KB and the rest is *cut*, while a picture has to arrive whole to be a
/// picture -- so this number is the whole file in this process's memory and in the tab's. A phone
/// photo is around 3 MB, a screenshot is under 1, a 12-bit scan can be 20; past 24 MB the honest
/// answer is the one the refusal gives, which is to open it where it lives.
const IMAGE_MAX: u64 = 24 * 1024 * 1024;

/// `GET /image?path=…`: one picture, as the bytes it is.
///
/// The type is read off the **file's own first bytes**, not off its name. `Content-Type` is a claim
/// a browser acts on -- it will hand the bytes to an image decoder and, for `image/svg+xml`, into a
/// document context -- and a name is whatever somebody typed. So `logo.png` that is really a JPEG is
/// served as `image/jpeg`, and `notes.txt` renamed to `logo.png` is refused with a sentence that says
/// so rather than sent to a decoder to fail in the tab. The page decides the same question the other
/// way round (by extension) because it has to pick a route before it can ask, and its fallback is
/// this route's refusal followed by `GET /file`.
///
/// What it will *not* serve is the thing §6 cares about: a path is a parameter, exactly as in
/// `/file`, so the route table stays literal and there is no traversal in a route.
fn serve_image(request: &Request, state: &State) -> Answer {
    let Some(raw) = request.query("path") else {
        return Answer::Once(Response::text(
            400,
            "Bad Request",
            "which image? this route takes ?path=<path>\n",
        ));
    };
    let Some(asked) = percent_decode(raw) else {
        return Answer::Once(Response::text(
            400,
            "Bad Request",
            "that path is not percent-encoded properly\n",
        ));
    };
    if asked.trim().is_empty() {
        return Answer::Once(Response::text(400, "Bad Request", "which image? ?path= was empty\n"));
    }
    let path = resolve(&asked, &state.cwd);
    match read_image(&path) {
        Ok(raw) => Answer::Raw(raw),
        Err(()) => Answer::Once(image_refusal(&asked, &path)),
    }
}

/// One picture: its bytes and the type they are.
fn read_image(path: &std::path::Path) -> Result<Raw, ()> {
    let metadata = std::fs::metadata(path).map_err(|_| ())?;
    if metadata.is_dir() || metadata.len() > IMAGE_MAX {
        return Err(());
    }
    // Bounded by the cap rather than by the file, for `read_preview`'s reason: the byte past the cap
    // is what tells this read there is more, and reading the rest of a 40 MB scan to find out is the
    // cost the cap exists to avoid.
    let mut file = std::fs::File::open(path).map_err(|_| ())?;
    let mut bytes = Vec::new();
    use std::io::Read as _;
    std::io::Read::take(&mut file, IMAGE_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > IMAGE_MAX {
        return Err(());
    }
    let content_type = image_kind(&bytes).ok_or(())?;
    Ok(Raw { content_type, body: bytes, extra: Vec::new() })
}

/// The refusal for a path `/image` would not serve, in words that name what the file is.
fn image_refusal(asked: &str, path: &std::path::Path) -> Response {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Response::text(
            400,
            "Bad Request",
            format!("{asked} is a directory, not an image\n"),
        ),
        Ok(metadata) if metadata.len() > IMAGE_MAX => Response::text(
            400,
            "Bad Request",
            format!(
                "{asked} is {} -- too big to show here; open it where it lives\n",
                bytes_label(metadata.len())
            ),
        ),
        // One sentence, and it names the formats in the sniffing table's own words so the two cannot
        // drift: a reader who pressed a path that looked like a picture is owed what this route
        // *would* draw. The page then asks `/file`, which is where a text file's own answer lives.
        Ok(_) => Response::text(
            400,
            "Bad Request",
            format!(
                "{asked} is not a picture this page can draw ({})\n",
                IMAGE_KINDS
            ),
        ),
        Err(e) => Response::text(404, "Not Found", format!("nothing at {asked}: {e}\n")),
    }
}

/// The picture formats this route knows, for the refusal's own sentence.
const IMAGE_KINDS: &str = "png, jpeg, gif, webp, bmp, ico, tiff, avif, heic and svg are";

/// What a file's first bytes say it is, or `None` when they say it is not a picture.
///
/// Magic bytes rather than an extension, for the reason `serve_image` gives. The list is what a
/// browser draws today without a plugin, which is what "mainstream" means here: PNG, JPEG, GIF,
/// WebP, BMP, ICO/CUR, TIFF, AVIF and HEIC/HEIF, and SVG -- which is text, so it is recognised by
/// looking at what it says rather than at how it starts.
///
/// Two of these are worth a word. **TIFF and HEIC are served although a browser may refuse them**:
/// the route's job is to say what the bytes are, and a tab that cannot decode one shows the panel's
/// own "cannot draw it" sentence with the `open` control beside it, which is a better answer than
/// this route guessing which browsers have which decoders. **SVG is served as `image/svg+xml`**, and
/// that is safe *here* rather than in general: an `<img>` is an image context, so a script inside the
/// file does not run -- and it is drawn from a blob URL, which is same-origin with the page but not
/// with the file.
fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8]| bytes.starts_with(sig);
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if starts(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some("image/gif");
    }
    // `RIFF` is four bytes of container; `WEBP` at offset 8 is what makes it a picture rather than a
    // sound or an AVI.
    if bytes.len() >= 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if starts(b"BM") {
        return Some("image/bmp");
    }
    // Icons and cursors are the same container with a different type field, and a browser draws both.
    if starts(&[0, 0, 1, 0]) || starts(&[0, 0, 2, 0]) {
        return Some("image/x-icon");
    }
    if starts(b"II\x2a\x00") || starts(b"MM\x00\x2a") {
        return Some("image/tiff");
    }
    // ISO base media file format: `ftyp` at 4, then the brand. A phone's photo is `heic`, a newer
    // one is `avif`, and both are the same container with different codecs inside.
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        match &bytes[8..12] {
            b"avif" | b"avis" => return Some("image/avif"),
            b"heic" | b"heix" | b"hevc" | b"heim" | b"heis" | b"mif1" | b"msf1" => {
                return Some("image/heic")
            }
            _ => {}
        }
    }
    if is_svg(bytes) {
        return Some("image/svg+xml");
    }
    None
}

/// Whether bytes are an SVG document, which is text and so cannot be recognised by a signature.
///
/// Read only from the first kilobyte, because an SVG's root element is the first thing in it unless
/// somebody put a licence comment there -- and this is a *routing* decision, not a validation: a file
/// that starts with `<?xml` or `<svg` and mentions `<svg` is handed to the browser's SVG reader, which
/// does its own parsing and shows nothing at all rather than something wrong.
fn is_svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
    let head = head.trim_start_matches('\u{feff}').trim_start();
    if !head.starts_with("<?xml") && !head.starts_with("<svg") {
        return false;
    }
    head.contains("<svg")
}

/// A relative path against the directory the run is working in; an absolute one as it is; and a
/// leading `~` as the home directory of whoever is running.
///
/// The rule is [`crate::config::resolve_path`], shared with the model's tools and a person's
/// `@name`, because the page is reading a path *the same model wrote in the transcript*: a `~/x`
/// that the `read` tool resolved one way and this route resolved another would show the reader two
/// different files under one name. What is local here is only which directory "relative" means --
/// the run's own working directory, not the process's.
fn resolve(asked: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    crate::config::resolve_path(cwd, asked)
}

/// `POST /open`: hand a path to the program this machine uses for it.
///
/// The other half of `GET /file`, and the same path asked a different question. *Read it here* is
/// answered for text the page can draw, and this is what is left: a directory, a file past the
/// preview cap, a PDF, a spreadsheet, an image. The refusals in `refusal` already tell a reader to
/// do this by hand ("open it where it lives"); this is the door.
///
/// **It starts a process**, which makes it the one route in this file that does, and that is the
/// whole reason for the shape around it:
///
/// - `readonly` refuses it, judged by the same all-or-nothing switch the tools are judged by. In a
///   readonly run the model cannot start a program, and a button that started one because a person
///   clicked text the model had written would be that program running anyway, one click removed.
/// - The decision is a pure function ([`open_plan`]) and the effect is one line. What is asserted
///   by the tests is the command line that is handed to `Command::spawn`, never a launched window:
///   a test that opened one would put a file manager on the screen of whoever ran it.
/// - Nothing is interpreted. The path is the argument of one program, `Command` runs it without a
///   shell of ours, and no part of the path is parsed for meaning -- the launcher decides, exactly
///   as the operating system would if a person pasted the path into a run box.
///
/// What it is *not* is a privilege boundary, and the honest version of that sentence belongs here
/// rather than in a document: in a run that is not readonly the model may run the same program
/// itself, through `bash` or `exec`, without asking anybody. What this adds is a person's click.
fn serve_open(body: &str, state: &State) -> Response {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(e) => return Response::text(400, "Bad Request", format!("expected a JSON body: {e}\n")),
    };
    let Some(asked) = parsed.get("path").and_then(|p| p.as_str()) else {
        return Response::text(400, "Bad Request", "expected {\"path\": \"...\"}\n");
    };
    if asked.trim().is_empty() {
        return Response::text(400, "Bad Request", "which path? the body's \"path\" was empty\n");
    }

    let path = resolve(asked, &state.cwd);
    let readonly = state
        .readonly
        .load(std::sync::atomic::Ordering::Relaxed);
    let plan = match open_plan(asked, &path, readonly, Platform::current()) {
        Ok(plan) => plan,
        Err(refusal) => return refusal,
    };

    // Detached, and never waited for here: a file manager or a viewer outlives this request by
    // hours, and a route that waited would be a request that never answered. The reaper thread is
    // not tidiness -- an unwaited child is a zombie on Unix until this process exits, and a page
    // can press this button as often as it likes.
    let mut child = match std::process::Command::new(plan.program)
        .args(&plan.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return Response::text(500, "Internal Server Error", format!("cannot open {asked}: {e}\n"))
        }
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Response::json(
        200,
        "OK",
        serde_json::json!({ "opened": asked, "with": plan.with }).to_string(),
    )
}

/// What opening a path would do: which program, which arguments, and what to call the program when
/// saying so.
#[derive(Debug)]
struct Plan {
    program: &'static str,
    args: Vec<std::ffi::OsString>,
    /// The launcher in the words the page shows a person ("opened with explorer"), which is the
    /// program's own name on Unix and the shell that runs `start` on Windows.
    with: &'static str,
}

/// Which launcher this operating system has, as a value rather than a `cfg!` at the call site.
///
/// A parameter of [`open_plan`] so that the three answers can be asserted on one machine: the
/// alternative -- three `#[cfg]` bodies that only the matching platform ever compiles -- is three
/// command lines of which at most one is ever checked by a test, and the other two would be written
/// once and never run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Platform {
    Windows,
    MacOs,
    Other,
}

impl Platform {
    fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        }
    }
}

/// The decision [`serve_open`] acts on, or the response that says why it will not.
///
/// Pure on purpose, and it is the whole of the route's thinking: the guard, whether there is
/// anything at the path, and the launcher's own command line. Nothing here spawns, so every refusal
/// and all three platforms are held by tests that run anywhere.
fn open_plan(
    asked: &str,
    path: &std::path::Path,
    readonly: bool,
    platform: Platform,
) -> Result<Plan, Response> {
    // The guard first, and before the path is looked at: a readonly run says the same sentence
    // whether or not there is anything at the path, because the answer is about the run rather
    // than about the file -- and "nothing at ..." would be a true sentence that suggests trying
    // another path.
    if readonly {
        return Err(Response::text(
            409,
            "Conflict",
            "this run is readonly, and opening a path starts a program on this machine: \
             /readonly off in the terminal, if that is what you want\n",
        ));
    }
    if let Err(e) = std::fs::metadata(path) {
        return Err(Response::text(404, "Not Found", format!("nothing at {asked}: {e}\n")));
    }

    // A directory is opened by the file manager and a file by whatever this machine has registered
    // for it, and the two are the *same* command on all three systems: the launcher asks the
    // operating system, which is the only thing that knows. That is deliberate -- a table of file
    // types in flint would be a copy of the registry that goes wrong quietly.
    let plan = match platform {
        Platform::Windows => Plan {
            program: "cmd",
            // `start` is a `cmd` builtin, so the shell is the launcher and `""` is the window title
            // it would otherwise take the quoted path for. The path is one argument of a program
            // that was given an argument list, so nothing in it is parsed as shell syntax.
            args: vec!["/C".into(), "start".into(), "".into(), path.into()],
            with: "explorer",
        },
        Platform::MacOs => Plan {
            program: "open",
            args: vec![path.into()],
            with: "open",
        },
        Platform::Other => Plan {
            program: "xdg-open",
            args: vec![path.into()],
            with: "xdg-open",
        },
    };
    Ok(plan)
}

/// A size in the words a person reads, for the one sentence that needs it.
fn bytes_label(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1024 * 1024 * 1024), ("MB", 1024 * 1024), ("KB", 1024)];
    for (unit, size) in UNITS {
        if bytes >= size {
            return format!("{} {unit}", bytes / size);
        }
    }
    format!("{bytes} bytes")
}

/// `%XX` back into bytes.
///
/// The page encodes a path with `encodeURIComponent`, so a space, a backslash, a colon or a
/// non-ASCII character arrives escaped and has to be unescaped before it means anything. `+` is
/// **not** a space: this is a path in a query string, not a form, and a file whose name has a plus
/// in it is a file like any other.
fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = bytes.get(at + 1..at + 3)?;
            let text = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---------------------------------------------------------------------------
// the socket
// ---------------------------------------------------------------------------
/// A running window: the port it is on and the token that opens it.
///
/// Held by the CLI for the life of the process, which is the whole of its lifetime -- the
/// listener is a window onto *this* run and has nothing to say once the run is over, so
/// there is nothing to shut down and no state to persist. Dropping this does not stop the
/// accept loop, deliberately: the process owns it, exactly as it owns the session file.
pub struct Window {
    port: u16,
    token: String,
}

impl Window {
    /// Bind loopback and start serving.
    ///
    /// `port` of zero means "whatever is free", and it is the default the CLI uses: the
    /// alternative is a fixed port that collides with whatever else is on loopback -- a local
    /// model server on 11434, 1234 or 8080 -- and the URL has to be printed anyway, so
    /// nothing is gained by making it guessable.
    ///
    /// The address is the literal `127.0.0.1`, never `0.0.0.0` and never a resolved
    /// "localhost" (§4.1). Remote access is not a feature of this program; it is a different
    /// program, and one with a much harder problem.
    pub async fn open(
        port: u16,
        session: Option<std::path::PathBuf>,
        live: Option<Arc<Live>>,
    ) -> Result<Window> {
        // A window with no run behind it -- `debug`, a test -- has no working directory of its own,
        // and the process's is the only answer that means anything for a relative path. It has no
        // write guard either, and `false` is the honest answer for something that is not a run:
        // nothing is being written by a window, and `POST /open` refuses over the missing path
        // rather than over a guard nobody set.
        let cwd = std::env::current_dir().unwrap_or_default();
        Window::open_following(
            port,
            Arc::new(Mutex::new(session)),
            live,
            None,
            cwd,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }

    /// The same, for a caller whose session can change under it.
    ///
    /// The REPL is that caller: `/new` and `/resume` move the run to a different file with
    /// the window still open, so the two of them share the path rather than the window
    /// taking a copy at bind time. It is also the caller that knows the working directory, which
    /// is a *different* fact from where the session file lives -- a conversation is filed under a
    /// key derived from the directory it was held in -- and the caller that owns the write guard,
    /// which it shares for the same reason: `/readonly` moves it while the page is open. See
    /// `State::readonly`.
    pub async fn open_following(
        port: u16,
        session: Arc<Mutex<Option<std::path::PathBuf>>>,
        live: Option<Arc<Live>>,
        input: Option<tokio::sync::mpsc::UnboundedSender<FromPage>>,
        cwd: std::path::PathBuf,
        readonly: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Window> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
        let port = listener
            .local_addr()
            .context("cannot read the port the listener got")?
            .port();
        let token = new_token();
        let state = Arc::new(State {
            token: token.clone(),
            port,
            session,
            live,
            input,
            cwd,
            readonly,
        });
        tokio::spawn(accept_loop(listener, state));
        Ok(Window { port, token })
    }

    /// The line to print: the URL a person pastes, with the token in it.
    ///
    /// The token is in the URL because there is nowhere else to put it -- a page that cannot
    /// authenticate itself cannot fetch anything, and asking a person to type a header is not
    /// a feature. It is why the response carries `Referrer-Policy: no-referrer`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/?token={}", self.port, self.token)
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// The browser view of this run, from the moment it is asked for.
///
/// **Two ways in, one handle.** `--web` opens a view before the first turn, and `/web` opens
/// one in the middle of a conversation -- which is the way a person actually reaches for it,
/// having discovered mid-session that the answer is long enough to want a real renderer. That
/// is why the REPL carries this rather than reading a flag once at startup.
///
/// The two halves are separable on purpose. `live` exists as soon as the view is *asked* for,
/// so a turn that begins while the socket is still being bound does not lose the frames it
/// produces; `window` appears when the bind succeeds, and until then there is nothing to
/// point a browser at. A `Viewer` with no window is not an error state -- it is a request
/// that has not been honoured yet.
pub struct Viewer {
    live: Arc<Live>,
    window: Option<Window>,
    /// Shared with whichever listener is serving it, so `/new` and `/resume` are visible to a
    /// browser that is already connected.
    session: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// The port `--port` asked for, kept so `/web` with no argument means what the flag meant.
    port: u16,
    /// The prompt the page types into. Absent for a run with no prompt, which is what makes
    /// `POST /message` answer `409` rather than swallowing a message nobody will read. See
    /// `State::input` for why this carries a [`FromPage`] rather than the REPL's own input event.
    input: Option<tokio::sync::mpsc::UnboundedSender<FromPage>>,
    /// The directory the run is working in, which is what `/file` resolves a relative path
    /// against. Held here rather than read from the listener's process, because the two are only
    /// the same by accident: the run's directory is `--cwd`'s answer. See `State::cwd`.
    cwd: std::path::PathBuf,
    /// The run's write guard, shared with whichever listener is serving this view.
    ///
    /// Here rather than in the listener because the run is what owns it: `--readonly` decides it
    /// before the page exists, and `/readonly` moves it afterwards. The page's copy is updated by
    /// the REPL where the agent is rebuilt -- see `set_readonly` -- so the route that launches a
    /// program and the tools that refuse to are never reading two different answers.
    readonly: Arc<std::sync::atomic::AtomicBool>,
}

impl Viewer {
    /// Ask for a view of `session`. Nothing is bound and nothing is listening yet.
    pub fn asked(
        port: u16,
        session: Option<std::path::PathBuf>,
        input: Option<tokio::sync::mpsc::UnboundedSender<FromPage>>,
        cwd: std::path::PathBuf,
        readonly: bool,
    ) -> Viewer {
        let live = Live::new();
        // Told which conversation it is about before anything can ask: a cursor is answered
        // out of that file, and a stream that does not know its own file can only ever answer
        // from the ring.
        live.serving(session.clone());
        Viewer {
            live,
            window: None,
            session: Arc::new(Mutex::new(session)),
            port,
            input,
            cwd,
            readonly: Arc::new(std::sync::atomic::AtomicBool::new(readonly)),
        }
    }

    /// The feed a turn copies its events into.
    pub fn live(&self) -> &Live {
        &self.live
    }

    /// Bind the listener if it is not bound, and return the URL to open.
    ///
    /// Idempotent, and it has to be: `/web` twice would otherwise leak a second listener on a
    /// second port, with the printed URL and the one a browser is already on disagreeing about
    /// which run it is showing. Asking again prints where it already is.
    pub async fn open(&mut self, port: u16) -> Result<String> {
        if let Some(window) = &self.window {
            return Ok(window.url());
        }
        let port = if port == 0 { self.port } else { port };
        let window = Window::open_following(
            port,
            Arc::clone(&self.session),
            Some(Arc::clone(&self.live)),
            self.input.clone(),
            self.cwd.clone(),
            Arc::clone(&self.readonly),
        )
        .await?;
        let url = window.url();
        self.window = Some(window);
        Ok(url)
    }

    /// Tell the view what the run's write guard is now.
    ///
    /// Called from the one place the agent is rebuilt, so every command that can change the guard
    /// -- `/readonly` today -- moves the page's answer with it, and every command that merely
    /// replaces the tools around the same conversation re-states the value that was already there.
    /// Costless when no window is open, which is the usual case.
    pub fn set_readonly(&self, readonly: bool) {
        self.readonly
            .store(readonly, std::sync::atomic::Ordering::Relaxed);
    }

    /// Tell the sidebar its list is out of date.
    ///
    /// Called after `/archive` and `/delete`. Cheap and safe to call when no window is open, and
    /// when none of the clients are looking -- a frame nobody reads is a frame nobody reads.
    pub fn list_changed(&self) {
        self.live.sessions_changed();
    }

    /// Point the view at a different conversation.
    ///
    /// Called after `/new` and `/resume`. The session file changes first and the readers are
    /// told second, so a page that re-reads on the `reset` reads the file the run is now
    /// writing rather than the one it just left.
    pub fn follow(&mut self, session: Option<std::path::PathBuf>) {
        if *self.session.lock().unwrap_or_else(|e| e.into_inner()) == session {
            return;
        }
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = session.clone();
        // The file first, then the readers: `restart` clears the ring, and a cursor that
        // arrives between the two must be read against the conversation the run is now
        // writing rather than the one it just left.
        self.live.serving(session);
        self.live.restart();
    }
}

/// Accept connections forever, one task each.
///
/// A failed accept is not fatal: it is a connection that went away between the kernel
/// queueing it and this process looking at it, which on a loopback socket is a browser
/// cancelling a request.
async fn accept_loop(listener: TcpListener, state: Arc<State>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let _ = serve(stream, &state).await;
        });
    }
}

/// Read one request head, answer it, close.
async fn serve(mut stream: TcpStream, state: &State) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break None;
        }
        buf.extend_from_slice(&chunk[..read]);
        // The end of the head is the whole of the request for every route but one, and the
        // bytes after it are the body of that one.
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break Some(at + 4);
        }
        if buf.len() > MAX_HEAD {
            let response = Response::text(431, "Request Header Fields Too Large", "too long\n");
            let _ = stream.write_all(&response.render()).await;
            let _ = stream.shutdown().await;
            return Ok(());
        }
    };

    // Only the head goes to the parser. A body that arrived in the same read would otherwise be
    // read as a header line, and a body is arbitrary text: `{"text":"a: b"}` is a JSON object
    // and also a plausible header.
    let (head, rest) = match head_end {
        Some(at) => (&buf[..at], buf[at..].to_vec()),
        None => (&buf[..], Vec::new()),
    };
    let Some(request) = Request::parse(&String::from_utf8_lossy(head)) else {
        let response = Response::text(400, "Bad Request", "not an HTTP/1.1 request\n");
        stream.write_all(&response.render()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    };

    let body = match read_body(&mut stream, &request, rest).await {
        Ok(body) => body,
        Err(response) => {
            stream.write_all(&response.render()).await?;
            let _ = stream.shutdown().await;
            return Ok(());
        }
    };

    match respond(&request, &body, state) {
        Answer::Once(response) => {
            stream.write_all(&response.render()).await?;
        }
        Answer::Raw(raw) => {
            stream.write_all(&raw.render()).await?;
        }
        Answer::Events { last, session } => {
            stream_events(stream, state, last, session).await?;
            return Ok(());
        }
    }
    // `Connection: close` is not a suggestion: the response is over when the socket is, and
    // that is what removes keep-alive, pipelining and request framing from this file (§6).
    let _ = stream.shutdown().await;
    Ok(())
}

/// The body, when the request says it has one.
///
/// Length-delimited and nothing else. No `Transfer-Encoding`, no chunked bodies, no
/// `Expect: 100-continue` -- §6's surface is small on purpose, and a page that sends a JSON
/// message can always say how long it is. A request that lies about the length, or stops early,
/// is refused rather than half-read: a truncated body is a truncated message.
async fn read_body(
    stream: &mut TcpStream,
    request: &Request,
    mut have: Vec<u8>,
) -> std::result::Result<String, Response> {
    let Some(length) = request.header("content-length") else {
        return Ok(String::new());
    };
    let length: usize = length.trim().parse().map_err(|_| {
        Response::text(400, "Bad Request", "Content-Length is not a number\n")
    })?;
    if length > MAX_BODY {
        return Err(Response::text(
            413,
            "Content Too Large",
            format!("a message may be at most {MAX_BODY} bytes\n"),
        ));
    }
    let mut chunk = [0u8; 8192];
    while have.len() < length {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| Response::text(400, "Bad Request", "the body did not arrive\n"))?;
        if read == 0 {
            return Err(Response::text(
                400,
                "Bad Request",
                "the body stopped before Content-Length did\n",
            ));
        }
        have.extend_from_slice(&chunk[..read]);
    }
    have.truncate(length);
    Ok(String::from_utf8_lossy(&have).to_string())
}

/// The headers of an event stream: everything a small response has, except a length.
///
/// No `Content-Length`, because the response ends when the client goes away rather than at a
/// byte count -- which is the one case §6's "a small, complete response" does not cover, and
/// the reason this route writes its own headers instead of going through `Response`. Everything
/// else it *is* `Response`'s, through [`SECURITY_HEADERS`], because the copy of those four that
/// used to live here was missing one of them.
///
/// A function rather than a `const &str` for that reason alone: a constant cannot splice another
/// constant into it at compile time without `concat!` seeing two literals, and the alternative --
/// writing the four out twice -- is the bug this replaced.
fn sse_headers() -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Connection: close\r\n\
         {SECURITY_HEADERS}\r\n"
    )
}

/// The words the status row is showing, sent once when a client connects.
///
/// A *named* event, and this one is the mirror of `SSE_RESET`: the run's vocabulary carries
/// changes, and a change a page never saw is not a change it can catch up on. What a turn is
/// waiting for is state, so it is sent as state -- once, outside the numbering, and applied
/// without a cursor.
const SSE_STATUS_SNAPSHOT: &str = "event: status\n";

/// The answer a turn in flight has written so far, as state rather than as change.
///
/// The second of the two things a cursor cannot express, and the sharper one. `/session` serves
/// the session file, and the file gets the assistant message when the turn **ends** -- so a page
/// that loads, reconnects, or is told `reset` in the middle of a turn would re-read a document
/// that does not contain the answer being written, and start the answer from the middle.
/// Measured by reloading a page during a turn.
///
/// The data is a JSON *string* rather than the raw text, unlike `status`: an answer has newlines
/// in it and a `data:` line does not.
const SSE_ANSWER_SNAPSHOT: &str = "event: answer\n";

/// What a page's controls could offer, as state rather than as change.
///
/// The third of them, and the one a page needs before it can draw anything with a choice in it:
/// which provider and model are in force, what each provider offers, and every setting a person can
/// change with the words it takes. Nothing
/// secret goes in it, which is why it is a frame a page can be handed on connect -- and why
/// `/provider key` is a form and not a picker.
const SSE_STATE_SNAPSHOT: &str = "event: state\n";

/// Tell a client its document is stale, so it re-reads the session.
///
/// A *named* SSE event rather than a line in the NDJSON vocabulary. Every `data:` line on
/// this stream is a fact about the run, and "your transcript is out of date" is a fact about
/// the *transport* -- the vocabulary is deliberately closed, and this does not belong in it.
const SSE_RESET: &str = "event: reset\ndata: {}\n\n";

/// Tell a client the run's job list is stale, so it re-reads `GET /jobs`.
///
/// The third named frame of the same kind, and empty for the same reason as `reset`: what changed
/// is a route's answer, and a copy of it on the stream would be a second answer.
const SSE_JOBS: &str = "event: jobs\ndata: {}\n\n";

/// How often the stream asks whether the job list changed.
///
/// The one change that happens while *nothing else is going on*: a background command ending has no
/// turn to ride on, so without a clock of its own the page's row would say "running" for up to a
/// heartbeat. Four seconds is short enough that a person sees it, and long enough that it is one
/// timer wake per open page. It is only the *fallback* -- a change is also noticed on every frame
/// this connection forwards, which is what makes a job appearing during a turn immediate.
const JOBS_POLL: std::time::Duration = std::time::Duration::from_secs(4);

/// Send the `jobs` frame if the run's job list has changed since this client last heard.
///
/// The revision is a counter rather than a copy of the list, so a client that missed a change still
/// hears about the next one, and two changes that arrive together are one frame -- which is what
/// the page needs, since it re-reads the whole route either way. The revision is compared here
/// rather than pushed from `tools`, because the tool code has no way to reach a connection and
/// should not grow one: the counter is the whole of the interface.
async fn send_jobs_if_changed(stream: &mut TcpStream, seen: &mut u64) -> Result<()> {
    let now = crate::tools::jobs_revision();
    if now != *seen {
        *seen = now;
        stream.write_all(SSE_JOBS.as_bytes()).await?;
    }
    Ok(())
}
/// The answer so far, if there is one, as the named event that says it is state.
async fn write_answer_snapshot(stream: &mut TcpStream, live: &Live) -> Result<()> {
    let answer = live.answer_so_far();
    if answer.is_empty() {
        return Ok(());
    }
    // Escaped, so it is one line whatever the answer contains.
    let data = serde_json::json!(answer).to_string();
    stream
        .write_all(format!("{SSE_ANSWER_SNAPSHOT}data: {data}\n\n").as_bytes())
        .await?;
    Ok(())
}

/// Write what a client missed, then everything as it happens, until it goes away.
async fn stream_events(
    mut stream: TcpStream,
    state: &State,
    last: Option<u64>,
    session: Option<String>,
) -> Result<()> {
    let Some(live) = &state.live else {
        let response = Response::text(404, "Not Found", "this run has no live feed\n");
        stream.write_all(&response.render()).await?;
        return Ok(());
    };

    // Subscribed *before* the headers go out, and that order is load-bearing: the client
    // reads `/session` as soon as it sees these headers, and anything emitted between the
    // subscription and that read is buffered for it. Subscribing afterwards would open a
    // window -- small, and exactly the kind that is never noticed until it is -- in which a
    // frame belongs to neither the file nor the stream.
    let (mut rx, catch) = live.follow(last, session.as_deref());

    stream.write_all(sse_headers().as_bytes()).await?;
    match catch {
        Catch::Reset => {
            // What it missed cannot be placed in the file it is showing either -- a cursor from
            // another conversation, a file that was rewritten under it -- so it re-reads the
            // session instead. The file is the truth and `GET /session` is the route for it;
            // keeping a durable log of the *stream* would be derived state, which is the one
            // thing this repository does not keep.
            stream.write_all(SSE_RESET.as_bytes()).await?;
        }
        Catch::Missed(frames) => {
            // A replay taken from the ring can contain a frame that the subscription, taken a
            // moment earlier, will also deliver: it was pushed in the gap between the two. The
            // ring's own numbering says which those are, and they are dropped here rather than
            // left for the page to recognise -- it applies what it is sent, because what it is
            // sent is what it does not have, so a duplicate would be applied twice.
            //
            // A replay read out of the file cannot have that problem: those frames are the
            // file's lines, which the channel never carries (`seq` zero is how a frame says it
            // was never in the ring), so nothing is drained for them. Neither is anything
            // drained for an empty replay: there was nothing to duplicate.
            let from_ring = frames.first().map(|frame| frame.seq > 0).unwrap_or(false);
            for frame in frames {
                stream.write_all(frame.render().as_bytes()).await?;
            }
            if from_ring {
                let newest = live.newest_seq();
                while let Ok(frame) = rx.try_recv() {
                    if frame.seq > newest {
                        // Pushed after the replay was taken, so it is not a duplicate: it goes
                        // out now, in order, and the loop below carries on from there.
                        stream.write_all(frame.render().as_bytes()).await?;
                        break;
                    }
                }
            }
        }
    }

    // Then the state, which is not in the ring and would not be found by a cursor: a page
    // opened in the middle of a turn has missed every status frame there has been. Sent
    // after the catch-up rather than before, because the catch-up is older than it is and
    // would overwrite it. An empty status is not sent -- a page with nothing to show is
    // already showing nothing.
    let status = live.current_status();
    if !status.is_empty() {
        stream
            .write_all(format!("{SSE_STATUS_SNAPSHOT}data: {status}\n\n").as_bytes())
            .await?;
    }
    // The state, which is the one of the three that is always sent when it is known: a page with
    // no status has nothing to wait for and a page with no answer is not missing anything, but a
    // page with no state has no options to draw, and every control §8 adds is drawn from it.
    let state = live.current_state();
    if !state.is_empty() {
        stream
            .write_all(format!("{SSE_STATE_SNAPSHOT}data: {state}\n\n").as_bytes())
            .await?;
    }
    write_answer_snapshot(&mut stream, live).await?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    // The first tick of an interval is immediate; a heartbeat now would be noise on a
    // connection that has this second been handed a response.
    heartbeat.tick().await;
    // A page that connects while jobs are running is told to read them, which is the same frame the
    // changes below send: zero as the starting "seen" revision means the first check always sends
    // one if this run has ever started a job, and a page with no jobs is not missing anything.
    let mut jobs_seen = 0u64;
    let mut jobs_tick = tokio::time::interval(JOBS_POLL);
    jobs_tick.tick().await;
    send_jobs_if_changed(&mut stream, &mut jobs_seen).await?;

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(frame) => {
                    stream.write_all(frame.render().as_bytes()).await?;
                    // Checked here as well as on the timer, so a job started by a tool call during
                    // this very turn appears while the turn is still running rather than up to four
                    // seconds later.
                    send_jobs_if_changed(&mut stream, &mut jobs_seen).await?;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // The client could not keep up and frames were dropped. Saying so is the
                    // only honest option -- the alternative is a transcript with a hole in
                    // it and nothing to indicate one -- and re-subscribing from *now* is
                    // what stops the frames still buffered from arriving twice.
                    stream.write_all(SSE_RESET.as_bytes()).await?;
                    // The two snapshots go with it, and the answer one is what makes this
                    // survivable: the frames that were dropped were the deltas of an answer
                    // the file does not have yet.
                    let status = live.current_status();
                    if !status.is_empty() {
                        stream
                            .write_all(format!("{SSE_STATUS_SNAPSHOT}data: {status}\n\n").as_bytes())
                            .await?;
                    }
                    let state = live.current_state();
                    if !state.is_empty() {
                        stream
                            .write_all(format!("{SSE_STATE_SNAPSHOT}data: {state}\n\n").as_bytes())
                            .await?;
                    }
                    write_answer_snapshot(&mut stream, live).await?;
                    // The job list goes with the answer snapshot: whatever the client missed may
                    // have included a job starting or ending, and a list it re-reads costs one
                    // request.
                    send_jobs_if_changed(&mut stream, &mut jobs_seen).await?;
                    rx = live.subscribe();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => {
                stream.write_all(b": ping\n\n").await?;
            }
            _ = jobs_tick.tick() => {
                // The fallback, and the only reason it exists: a background command that ended
                // while this connection was quiet. Everything else is noticed above.
                send_jobs_if_changed(&mut stream, &mut jobs_seen).await?;
            }
        }
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            token: "0123456789abcdef0123456789abcdef".to_string(),
            port: 7777,
            session: Arc::new(Mutex::new(None)),
            live: None,
            input: None,
            // A window with no run behind it: the process's own directory is the only answer, and
            // the `/file` tests that care build their own state over a scratch directory.
            cwd: std::env::temp_dir(),
            // Not readonly, which is what a fixture without a run can honestly be: see
            // `Window::open`. The `/open` guard tests build a readonly state of their own.
            readonly: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Ask the pure router a question, as bytes a client would have sent.
    fn ask(head: &str, state: &State) -> Response {
        let request = Request::parse(head).unwrap_or_else(|| panic!("fixture must parse: {head:?}"));
        respond(&request, "", state).once()
    }

    /// A request from our own page, with the token in the header.
    fn ours(target: &str) -> String {
        format!(
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nX-Flint-Token: {}\r\n\r\n",
            state().token
        )
    }

    fn body(response: &Response) -> String {
        response.body.clone()
    }

    /// One of the headers a *route* adds, as opposed to the ones the transport always sends.
    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .extra
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_request_from_our_page_gets_the_view() {
        let state = state();
        let response = ask(&ours("/"), &state);
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "text/html; charset=utf-8");
        // Served from the embedded copy, so there is no second version to drift.
        assert_eq!(response.body, VIEW_HTML);
    }

    /// §4.2: without the token, nothing. With a wrong one, the same nothing.
    #[test]
    fn the_token_is_required_and_a_wrong_one_is_not_better_than_none() {
        let state = state();

        let none = ask("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", &state);
        assert_eq!(none.status, 403, "no token must be refused");

        let wrong = ask(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nX-Flint-Token: 0123456789abcdef0123456789abcdee\r\n\r\n",
            &state,
        );
        assert_eq!(wrong.status, 403, "a token one character off must be refused");

        // A prefix of the real token is the mistake a person makes by hand, and it must not
        // be accepted by a comparison that stops at the shortest of the two.
        let short = ask(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nX-Flint-Token: 0123\r\n\r\n",
            &state,
        );
        assert_eq!(short.status, 403, "a shorter token must be refused");
    }

    /// §4.3: the DNS-rebinding defence. A name that resolves here is still not this address.
    #[test]
    fn a_request_that_did_not_come_to_this_address_is_refused() {
        let state = state();
        for host in [
            None,
            Some("evil.example:7777"),      // the rebinding case: resolves to 127.0.0.1
            Some("localhost:7776"),          // right name, wrong port
            Some("127.0.0.1"),               // no port at all
            Some("0.0.0.0:7777"),            // bound everywhere means reachable everywhere
            Some("[::1]:7777"),
        ] {
            let host_line = host.map(|h| format!("Host: {h}\r\n")).unwrap_or_default();
            let head = format!(
                "GET / HTTP/1.1\r\n{host_line}X-Flint-Token: {}\r\n\r\n",
                state.token
            );
            // A request with no Host at all, in HTTP/1.1, is malformed as well as suspicious;
            // either way it must not be served.
            let response = ask(&head, &state);
            assert_eq!(
                response.status, 403,
                "Host {host:?} must be refused, and it was served: {:?}",
                body(&response)
            );
        }

        // The two names that *are* this listener, which is what makes the list above mean
        // something rather than testing that everything is refused.
        for host in ["127.0.0.1:7777", "localhost:7777", "LOCALHOST:7777"] {
            let head = format!(
                "GET / HTTP/1.1\r\nHost: {host}\r\nX-Flint-Token: {}\r\n\r\n",
                state.token
            );
            assert_eq!(ask(&head, &state).status, 200, "Host {host} is this listener");
        }
    }

    /// §4.4: absent is fine (curl), foreign is not.
    #[test]
    fn a_post_from_another_origin_is_refused() {
        let state = state();
        let request = |origin: Option<&str>| {
            let line = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
            format!(
                "POST /message HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n{line}X-Flint-Token: {}\r\n\r\n",
                state.token
            )
        };

        assert!(
            origin_is_ours(None, 7777),
            "curl sends no Origin and must still work"
        );
        for origin in [
            "http://evil.example",
            "https://127.0.0.1:7777",  // right address, wrong scheme: a different origin
            "http://localhost:7776",
            "null",                     // what a `file://` page sends
        ] {
            let response = ask(&request(Some(origin)), &state);
            assert_eq!(response.status, 403, "Origin {origin} must be refused");
        }
        for origin in ["http://127.0.0.1:7777", "http://localhost:7777"] {
            assert!(origin_is_ours(Some(origin), 7777), "{origin} is us");
        }
    }

    /// The query string carries the token on `/` and nowhere else.
    ///
    /// `/` is the route a person pastes into a browser, so the token has to travel in the
    /// URL. Every other route is called by the page's own `fetch`, which can set a header --
    /// and a query string is the one that ends up in logs, history and shared links.
    #[test]
    fn the_query_string_carries_the_token_only_for_the_view() {
        let state = state();
        let by_query = |target: &str| {
            format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n")
        };

        assert_eq!(ask(&by_query("/?token=0123456789abcdef0123456789abcdef"), &state).status, 200);
        // Same token, a route that changes what a reader sees: the header would have
        // worked, the query does not.
        assert_eq!(ask(&by_query("/session?token=0123456789abcdef0123456789abcdef"), &state).status, 403);
    }

    fn with_session(path: std::path::PathBuf) -> State {
        State { session: Arc::new(Mutex::new(Some(path))), ..state() }
    }

    /// A state with the prompt attached, and the receiving end of it.
    fn with_input() -> (State, tokio::sync::mpsc::UnboundedReceiver<FromPage>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<FromPage>();
        (State { input: Some(tx), ..state() }, rx)
    }

    /// A `POST` from our own page, with a body.
    fn post(target: &str, json: &str) -> String {
        format!(
            "POST {target} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\
             X-Flint-Token: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{json}",
            state().token,
            json.len()
        )
    }

    /// `POST /message` is the only route that can make something *happen*, so the checks §4
    /// asks for are asserted against it directly rather than only against the reads.
    #[test]
    fn a_message_is_queued_into_the_prompt() {
        let (state, mut rx) = with_input();
        let json = r#"{"text":"hello from the page"}"#;
        let request = Request::parse(&post("/message", json)).expect("fixture parses");
        let response = respond(&request, json, &state).once();
        assert_eq!(response.status, 202, "{}", body(&response));
        assert_eq!(
            rx.try_recv().expect("the message must reach the prompt"),
            FromPage::Line("hello from the page".to_string())
        );
    }

    /// The two routes carry the same text and different kinds, and the kind is the whole of the
    /// difference: a typed line prints here, a report does not. Asserted at the route, because that
    /// is where the kind is chosen, and a route that sent `Line` for both would be a page whose
    /// panels printed into the terminal -- a failure that is invisible until somebody reads the
    /// same listing twice.
    #[test]
    fn a_report_is_queued_as_a_report_and_a_message_as_a_line() {
        let (state, mut rx) = with_input();
        for (route, expected) in [
            ("/message", FromPage::Line("/config".to_string())),
            ("/report", FromPage::Report("/config".to_string())),
        ] {
            let json = r#"{"text":"/config"}"#;
            let request = Request::parse(&post(route, json)).expect("fixture parses");
            let response = respond(&request, json, &state).once();
            assert_eq!(response.status, 202, "{route}: {}", body(&response));
            assert_eq!(rx.try_recv().expect("queued"), expected, "{route}");
        }
    }

    /// A report without the token is refused and never queued, like the message route.
    ///
    /// The report route only *reads*, and that is not a reason to leave the credential check off: a
    /// page on another origin that could post here could make this process print a listing into a
    /// file or a pipe, and the token is the only thing that says which page may ask.
    #[test]
    fn a_report_without_the_token_is_refused_and_never_queued() {
        let (state, mut rx) = with_input();
        let json = r#"{"text":"/config"}"#;
        let head = "POST /report HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Length: 18\r\n\r\n";
        let request = Request::parse(&format!("{head}{json}")).expect("fixture parses");
        let response = respond(&request, json, &state).once();
        assert_eq!(response.status, 403, "{}", body(&response));
        assert!(rx.try_recv().is_err(), "a report must not be queued without the token");
    }

    /// The text is carried untouched, because meaning is decided in exactly one place.
    ///
    /// A slash command from the sidebar is the case that matters: the page has no idea what
    /// `/resume 3` does, and must not. If this route ever began interpreting what it carries,
    /// there would be two places that decide what a typed line means, and they would drift.
    #[test]
    fn a_message_is_not_interpreted_here() {
        let (state, mut rx) = with_input();
        for text in ["/resume 3", "!ls -la", "--web", "  spaced  "] {
            let json = serde_json::json!({ "text": text }).to_string();
            let request = Request::parse(&post("/message", &json)).expect("fixture parses");
            let response = respond(&request, &json, &state).once();
            assert_eq!(response.status, 202, "{text}: {}", body(&response));
            assert_eq!(rx.try_recv().expect("queued"), FromPage::Line(text.to_string()));
        }
    }

    #[test]
    fn a_message_without_the_token_is_refused_and_never_queued() {
        let (state, mut rx) = with_input();
        let head = "POST /message HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Length: 20\r\n\r\n";
        let request = Request::parse(head).expect("fixture parses");
        let json = r#"{"text":"let me in"}"#;
        assert_eq!(respond(&request, json, &state).once().status, 403);
        assert!(
            rx.try_recv().is_err(),
            "a refused request must not have queued anything"
        );
    }

    /// A page the user did not write cannot ask flint to do something.
    #[test]
    fn a_message_from_another_origin_is_refused() {
        let (state, mut rx) = with_input();
        let json = r#"{"text":"do as I say"}"#;
        let head = post("/message", json).replace(
            "Host: 127.0.0.1:7777",
            "Host: 127.0.0.1:7777\r\nOrigin: http://evil.example",
        );
        let request = Request::parse(&head).expect("fixture parses");
        assert_eq!(respond(&request, json, &state).once().status, 403);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_message_that_is_not_a_message_is_refused() {
        let (state, mut rx) = with_input();
        for (json, why) in [
            ("not json at all", "not JSON"),
            (r#"{"text":42}"#, "text is not a string"),
            (r#"{}"#, "no text"),
            (r#"{"text":"   "}"#, "only whitespace"),
        ] {
            let request = Request::parse(&post("/message", json)).expect("fixture parses");
            let response = respond(&request, json, &state).once();
            assert_eq!(response.status, 400, "{why}: {}", body(&response));
        }
        assert!(rx.try_recv().is_err(), "a refused message must not be queued");
    }

    /// A run with no prompt says so, rather than accepting a message nobody will read.
    ///
    /// This is the `-p` case: `--web` with a one-shot prompt serves a view, and a page that
    /// typed into it would otherwise get a cheerful `202` for a message that goes nowhere.
    #[test]
    fn a_run_with_no_prompt_refuses_a_message() {
        let json = r#"{"text":"hello"}"#;
        let request = Request::parse(&post("/message", json)).expect("parses");
        let response = respond(&request, json, &state()).once();
        assert_eq!(response.status, 409);
        assert!(body(&response).contains("no prompt"), "{}", body(&response));
    }

    /// The sidebar's numbers have to be the numbers `/resume` accepts.
    ///
    /// Asserted against `session::list` rather than against a fixture, because a fixture would
    /// be a second opinion about the ordering and the whole guarantee is that there is one.
    #[test]
    fn the_session_list_is_the_one_resume_numbers() {
        let dir = std::env::temp_dir().join(format!("flint-web-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let older = dir.join("100-1.jsonl");
        let newer = dir.join("200-2.jsonl");
        for (path, id, asked) in [
            (&older, "100-1", "the older question"),
            (&newer, "200-2", "the newer question"),
        ] {
            std::fs::write(
                path,
                format!(
                    "{{\"type\":\"meta\",\"v\":2,\"id\":\"{id}\",\"cwd\":\"/tmp\",\
                     \"provider\":\"p\",\"model\":\"m\"}}\n\
                     {{\"type\":\"chat\",\"message\":{{\"role\":\"user\",\
                     \"content\":\"{asked}\"}}}}\n"
                ),
            )
            .expect("write");
        }
        // Newest first is decided by mtime, so the order has to be forced rather than assumed.
        let now = std::time::SystemTime::now();
        set_modified(&older, now - std::time::Duration::from_secs(60));
        set_modified(&newer, now);

        // A child run's conversation, written by the real writer so the fixture cannot disagree with
        // the layout. It is a session like any other and it is not one of the person's: reported from a
        // real session, where a `task` child's conversation was in this sidebar beside the parent's,
        // with nothing to say which was which.
        let mut child = crate::session::SessionWriter::create(
            &dir,
            std::path::Path::new("/tmp"),
            "p",
            "m",
            Some("100-1"),
        )
        .expect("a child session");
        child.title("the child's conversation").expect("title");
        assert!(child.path().exists(), "the child's file was not written");

        let expected = crate::session::list(&dir).expect("list");
        assert_eq!(
            expected.len(),
            2,
            "the sidebar is not just the person's conversations: {expected:?}"
        );
        assert_eq!(expected[0].0, "200-2", "newest first");

        let response = serve_sessions_in(&dir, &state());
        assert_eq!(response.status, 200, "{}", body(&response));
        let parsed: serde_json::Value =
            serde_json::from_str(&body(&response)).expect("the list is JSON");
        let items = parsed["sessions"].as_array().expect("an array");
        assert_eq!(items.len(), 2);
        for (index, (id, label)) in expected.iter().enumerate() {
            assert_eq!(items[index]["n"], index + 1, "1-based, and in the same order");
            assert_eq!(items[index]["id"], id.as_str());
            assert_eq!(items[index]["label"], label.as_str());
            assert_eq!(items[index]["current"], false, "nothing is open in a bare state");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_open_conversation_is_marked_in_the_list() {
        let dir = std::env::temp_dir().join(format!("flint-web-current-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("300-3.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"meta\",\"v\":2,\"id\":\"300-3\",\"cwd\":\"/tmp\",\
             \"provider\":\"p\",\"model\":\"m\"}\n",
        )
        .expect("write");

        let response = serve_sessions_in(&dir, &with_session(path));
        let parsed: serde_json::Value =
            serde_json::from_str(&body(&response)).expect("the list is JSON");
        assert_eq!(parsed["sessions"][0]["current"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn set_modified(path: &std::path::Path, when: std::time::SystemTime) {
        let handle = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open to set mtime");
        handle.set_modified(when).expect("set mtime");
    }


    /// `/session` is the file, byte for byte -- not a summary and not a second format.
    #[test]
    fn the_session_route_serves_the_file_itself() {
        let dir = std::env::temp_dir().join(format!("flint-web-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("1.jsonl");
        // Shaped like a real one, including the trailing newline `writeln!` leaves.
        let body = "{\"type\":\"meta\",\"v\":2,\"id\":\"1\"}\n{\"type\":\"chat\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        std::fs::write(&path, body).expect("write");

        let state = with_session(path.clone());
        let response = ask(&ours("/session"), &state);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, body, "the route must not reformat anything");
        assert!(
            response.content_type.starts_with("application/x-ndjson"),
            "the page reads it as lines of JSON: {}",
            response.content_type
        );

        // Appended to between requests, and the next request sees it. This is the whole
        // reason the route re-reads the file instead of caching what it read once.
        let mut grown = body.to_string();
        grown.push_str("{\"type\":\"chat\",\"message\":{\"role\":\"assistant\",\"content\":\"hello\"}}\n");
        std::fs::write(&path, &grown).expect("append");
        assert_eq!(ask(&ours("/session"), &state).body, grown);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A last line with no newline is a read that caught the file mid-append, and it is
    /// served as it is.
    ///
    /// The tempting fix is to drop it. That would be wrong: `session::load` reads such a
    /// line, so dropping it here would make the browser show a conversation one message
    /// shorter than the terminal's -- the divergence this whole design exists to prevent.
    #[test]
    fn a_half_written_last_line_is_served_rather_than_hidden() {
        let dir = std::env::temp_dir().join(format!("flint-web-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("1.jsonl");
        let torn = "{\"type\":\"meta\",\"v\":2}\n{\"type\":\"chat\",\"mess";
        std::fs::write(&path, torn).expect("write");

        let response = ask(&ours("/session"), &with_session(path), );
        assert_eq!(response.status, 200);
        assert_eq!(response.body, torn);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run that writes no session says so, rather than serving an empty conversation.
    #[test]
    fn a_run_without_a_session_says_so() {
        let response = ask(&ours("/session"), &state());
        assert_eq!(response.status, 404);
        assert!(response.body.contains("not writing a session"), "{}", response.body);
    }

    /// A session file that does not exist *yet* is an empty conversation, not an error.
    ///
    /// Found by driving the page in a real browser rather than by reading this file: a run with
    /// `--web` produces the page's controls from the `state` frame, and the page cannot finish
    /// drawing until `/session` has answered. The session is named the moment the run starts --
    /// the id is part of the run, and a switch prints it -- while the *file* is created by the
    /// first thing said in it, which is a deliberate rule from `session.rs` (opening flint and
    /// typing nothing leaves nothing behind). So the first thing a fresh run's browser asks for
    /// is the one path that does not exist, and this route answered 500 with the raw
    /// `os error 3`, which is a page that never draws a single control and a person who sees a
    /// broken view rather than a new conversation.
    ///
    /// An empty body is the honest answer and it is the same one an empty file would get: the
    /// page is a reader of the file, and a file with nothing in it reads as a conversation with
    /// nothing in it. Any *other* read failure still goes to the 500 below, because a permission
    /// problem or a bad path is a real fault and hiding it would be worse than showing it.
    #[test]
    fn a_session_file_that_does_not_exist_yet_is_an_empty_conversation() {
        let dir = std::env::temp_dir().join(format!("flint-web-unwritten-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        // The directory as well as the file is missing, which is the case a fresh run is in: the
        // run's directory under `sessions/` is created with the file.
        let path = dir.join("subdir").join("1.jsonl");
        assert!(!path.exists());

        let response = ask(&ours("/session"), &with_session(path));
        assert_eq!(
            response.status, 200,
            "a fresh run's page is refused the conversation it has not started yet: {}",
            response.body
        );
        assert_eq!(response.body, "", "there is nothing to read, so there is nothing to serve");
        assert!(response.content_type.starts_with("application/x-ndjson"), "{}", response.content_type);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // `/file`: the one route that reads the disk, and only what a person pointed at
    // -----------------------------------------------------------------------

    /// A scratch directory under the platform's temp directory, the way every suite here makes one.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flint-web-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    /// A state whose working directory is `dir`: what a relative path in a tool result means.
    fn working_in(dir: &std::path::Path) -> State {
        State {
            cwd: dir.to_path_buf(),
            ..state()
        }
    }

    fn written(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("scratch directory");
        }
        std::fs::write(&path, body).expect("scratch file");
        path
    }

    /// `encodeURIComponent`, near enough: what the page does to a path before it goes in the
    /// query string, so the route's decoding is tested against the encoding it will really see.
    fn encoded(text: &str) -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    /// The three launchers, as command lines.
    ///
    /// All three on every machine, which is the reason `Platform` is a parameter rather than a
    /// `cfg!`: two of these three are never compiled on the machine that runs the suite, so without
    /// this they would be written once and never executed anywhere. What is asserted is the whole
    /// command line -- program and arguments -- because that is exactly what is handed to
    /// `Command::spawn` a few lines below, and it is the only part of the launch a test can hold
    /// without opening a window on somebody's screen.
    #[test]
    fn each_platform_is_opened_by_the_program_it_has() {
        let dir = scratch("open-platform");
        let file = written(&dir, "notes.txt", "hello");

        let plan = open_plan("notes.txt", &file, false, Platform::Windows).expect("windows opens it");
        assert_eq!(plan.program, "cmd");
        assert_eq!(
            plan.args,
            vec![
                std::ffi::OsString::from("/C"),
                std::ffi::OsString::from("start"),
                // The empty title, without which `start` takes the quoted path for a window title
                // and opens nothing at all.
                std::ffi::OsString::from(""),
                file.clone().into_os_string(),
            ],
            "the Windows launcher is `start`, and the path is one argument of it"
        );
        assert_eq!(plan.with, "explorer");

        let plan = open_plan("notes.txt", &file, false, Platform::MacOs).expect("macos opens it");
        assert_eq!((plan.program, plan.args), ("open", vec![file.clone().into_os_string()]));

        let plan = open_plan("notes.txt", &file, false, Platform::Other).expect("linux opens it");
        assert_eq!(
            (plan.program, plan.args),
            ("xdg-open", vec![file.clone().into_os_string()])
        );

        // A directory goes through the same command on all three: the launcher asks the operating
        // system, which is the only thing that knows what a directory is opened by. A table of file
        // types in flint would be a copy of the registry.
        let plan = open_plan("open-platform", &dir, false, Platform::Other).expect("a directory opens");
        assert_eq!(plan.args, vec![dir.clone().into_os_string()]);
    }

    /// The guard, and it comes before the path is looked at.
    ///
    /// Two things are being held, and the second is the one that would rot silently: that a
    /// readonly run refuses, and that it refuses *the same way for a path that does not exist* --
    /// "nothing at ..." would be a true sentence that suggests trying another path, when the answer
    /// is about the run.
    #[test]
    fn a_readonly_run_refuses_to_open_a_path_at_all() {
        let dir = scratch("open-guarded");
        let file = written(&dir, "notes.txt", "hello");

        for (what, path) in [("a file that is there", file.clone()), ("a path that is not", dir.join("gone.txt"))] {
            let refused = open_plan("notes.txt", &path, true, Platform::Other)
                .expect_err(&format!("{what} must be refused in a readonly run"));
            assert_eq!(refused.status, 409, "{what}");
            assert_eq!(
                body(&refused),
                "this run is readonly, and opening a path starts a program on this machine: \
                 /readonly off in the terminal, if that is what you want\n",
                "{what}"
            );
        }
    }

    /// And the route reads that guard from the state the REPL keeps moving, not from a copy taken
    /// when the window was bound.
    ///
    /// The failure this exists for is a page that goes on launching programs after `/readonly on`
    /// in the terminal, which is the model's own guard read backwards: the run refuses the tool and
    /// the browser accepts the click.
    #[test]
    fn the_open_route_reads_the_guard_the_run_is_holding_now() {
        let dir = scratch("open-state");
        // A path that does not exist, so that nothing is ever launched whichever way this goes --
        // every answer below is a refusal, and the one that differs is the guard's.
        let gone = dir.join("gone.txt");
        let json = format!(r#"{{"path":{}}}"#, serde_json::json!(gone.to_string_lossy()));

        let guard = state().readonly.clone();
        let mut state = working_in(&dir);
        state.readonly = guard.clone();
        let request = Request::parse(&post("/open", &json)).expect("fixture parses");
        let response = respond(&request, &json, &state).once();
        assert_eq!(response.status, 404, "an unguarded run asks the path: {}", body(&response));

        // The same listener, told what `/readonly on` means, refuses the same request.
        guard.store(true, std::sync::atomic::Ordering::Relaxed);
        let response = respond(&request, &json, &state).once();
        assert_eq!(response.status, 409, "{}", body(&response));
        assert!(body(&response).contains("readonly"), "{}", body(&response));

        // And `/readonly off` gives it back, so the mirror is a guard and not a one-way latch.
        guard.store(false, std::sync::atomic::Ordering::Relaxed);
        let response = respond(&request, &json, &state).once();
        assert_eq!(response.status, 404, "{}", body(&response));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the route says when it cannot even get to a path: no body, no path, an empty one.
    #[test]
    fn the_open_route_asks_for_a_path_in_the_bodys_own_words() {
        for (json, status, said) in [
            ("not json at all", 400, "expected a JSON body: "),
            ("{}", 400, "expected {\"path\": \"...\"}\n"),
            (r#"{"path":""}"#, 400, "which path? the body's \"path\" was empty\n"),
            (r#"{"path":"   "}"#, 400, "which path? the body's \"path\" was empty\n"),
        ] {
            let request = Request::parse(&post("/open", json)).expect("fixture parses");
            let response = respond(&request, json, &state()).once();
            assert_eq!(response.status, status, "{json}: {}", body(&response));
            assert!(
                body(&response).starts_with(said),
                "{json}: expected {said:?}, got {:?}",
                body(&response)
            );
        }
    }

    /// A path that is not there is not a refusal of the *request*: it is a 404 with the operating
    /// system's own words, the same shape `GET /file` uses -- and it is what proves the path is
    /// looked at before anything is launched, since the only thing after that check is `spawn`.
    #[test]
    fn a_path_that_is_not_there_is_reported_before_anything_is_started() {
        let dir = scratch("open-missing");
        let json = r#"{"path":"nowhere/at/all.txt"}"#;
        let request = Request::parse(&post("/open", json)).expect("fixture parses");
        let response = respond(&request, json, &working_in(&dir)).once();
        assert_eq!(response.status, 404, "{}", body(&response));
        assert!(
            body(&response).starts_with("nothing at nowhere/at/all.txt: "),
            "the refusal names the path as it was asked for: {}",
            body(&response)
        );
    }

    /// The list the page's jobs panel is drawn from, on the wire.
    ///
    /// The *contents* belong to `tools::tests` (a job is a job, and only that module can start one);
    /// what is asserted here is the route's half: it answers with an object holding an array, it
    /// needs the token like everything else, and it is a read -- `POST /jobs` is not a route.
    #[test]
    fn the_jobs_route_answers_with_the_runs_own_jobs() {
        let state = state();
        let response = ask(&ours("/jobs"), &state);
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.content_type, "application/json; charset=utf-8");
        let parsed: serde_json::Value =
            serde_json::from_str(&response.body).expect("the route answers with JSON");
        assert!(
            parsed["jobs"].is_array(),
            "the page iterates this, so it has to be a list: {}",
            response.body
        );

        let none = ask(
            "GET /jobs HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n",
            &state,
        );
        assert_eq!(none.status, 403, "a job's command line is not public");

        let posted = ask(
            &format!(
                "POST /jobs HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nX-Flint-Token: {}\r\n\r\n",
                state.token
            ),
            &state,
        );
        assert_eq!(posted.status, 404, "there is nothing to write here");
    }

    /// One file the transcript named, opened. This is the route §12 exists for.
    #[test]
    fn a_file_the_transcript_names_can_be_opened() {
        let dir = scratch("file-open");
        let path = written(&dir, "src/main.rs", "fn main() {}\n");

        let relative = ask(&ours(&format!("/file?path={}", encoded("src/main.rs"))), &working_in(&dir));
        assert_eq!(relative.status, 200, "{}", relative.body);
        assert_eq!(relative.body, "fn main() {}\n", "the file's own bytes, not a rendering");
        assert_eq!(relative.content_type, "text/plain; charset=utf-8");
        assert_eq!(header(&relative, "X-Flint-Cut"), None, "nothing was left out");

        // The absolute path the terminal prints works the same way: that is what the transcript
        // most often contains, because it is what flint's own tools report.
        let absolute = ask(
            &ours(&format!("/file?path={}", encoded(&path.display().to_string()))),
            &working_in(&dir),
        );
        assert_eq!(absolute.status, 200, "{}", absolute.body);
        assert_eq!(absolute.body, "fn main() {}\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `grep` prints a hit as `path:12:3: text`, and that whole token is what a person clicks. The
    /// line is a fact about where in the file, so it is reported rather than thrown away.
    #[test]
    fn a_path_carrying_a_line_number_opens_the_file_and_names_the_line() {
        let dir = scratch("file-line");
        written(&dir, "notes.txt", "one\ntwo\nthree\n");

        let with_line = ask(&ours(&format!("/file?path={}", encoded("notes.txt:2"))), &working_in(&dir));
        assert_eq!(with_line.status, 200, "{}", with_line.body);
        assert_eq!(with_line.body, "one\ntwo\nthree\n");
        assert_eq!(header(&with_line, "X-Flint-Line"), Some("2"));

        let with_column = ask(&ours(&format!("/file?path={}", encoded("notes.txt:3:1"))), &working_in(&dir));
        assert_eq!(with_column.status, 200, "{}", with_column.body);
        assert_eq!(header(&with_column, "X-Flint-Line"), Some("3"));

        // A file that really is called `notes.txt:2` is not a thing on Windows, but a *path* that
        // has a colon in it is (`C:\...`), so the fallback only ever runs after the literal path
        // has failed -- and when neither is there, the answer is still that there is nothing.
        let nowhere = ask(&ours(&format!("/file?path={}", encoded("notes.txt:999"))), &working_in(&dir));
        assert_eq!(nowhere.status, 200, "the line number is not part of the filename: {}", nowhere.body);
        assert_eq!(header(&nowhere, "X-Flint-Line"), Some("999"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every way this can fail has a sentence, because a click that does nothing is the thing a
    /// person cannot act on.
    #[test]
    fn a_missing_file_a_directory_and_a_binary_each_say_what_they_are() {
        let dir = scratch("file-refusals");
        std::fs::create_dir_all(dir.join("sub")).expect("scratch dir");

        let missing = ask(&ours(&format!("/file?path={}", encoded("nope.txt"))), &working_in(&dir));
        assert_eq!(missing.status, 404);
        assert!(missing.body.contains("nope.txt"), "{}", missing.body);

        let directory = ask(&ours(&format!("/file?path={}", encoded("sub"))), &working_in(&dir));
        assert_eq!(directory.status, 400, "{}", directory.body);
        assert!(directory.body.contains("directory"), "{}", directory.body);
        // ...and the refusal says *which kind* of thing it was in a header as well, because the page
        // acts on it: a directory is read through `GET /dir`, and a page that had to read the sentence
        // to know that would be a page parsing prose it wrote itself.
        assert_eq!(
            header(&directory, "X-Flint-Dir"),
            Some("1"),
            "the page cannot tell a directory from a binary without this: {}",
            directory.body
        );

        std::fs::write(dir.join("blob.bin"), [0x00, 0xFF, 0xFE, 0x41]).expect("scratch binary");
        let binary = ask(&ours(&format!("/file?path={}", encoded("blob.bin"))), &working_in(&dir));
        assert_eq!(binary.status, 400, "{}", binary.body);
        assert!(binary.body.contains("text"), "{}", binary.body);

        // Asked with no path at all: a question answered, not a missing route.
        let unsaid = ask(&ours("/file"), &working_in(&dir));
        assert_eq!(unsaid.status, 400, "{}", unsaid.body);
        assert!(unsaid.body.contains("path"), "{}", unsaid.body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory is a reading of its own, and the route that serves it is `GET /dir`.
    ///
    /// Reported directly, 2026-09-23: *pressing a directory does not go anywhere, and a directory with
    /// a space in its name is not recognised at all.* A directory path in a transcript is a button --
    /// the argument of a `list`, the path a `glob` walked, something the model wrote -- and pressing
    /// one used to fill the panel with "is a directory, not a file". The name was the reason a
    /// directory with a space was never a button in the first place: the scanner reads a *token*, and
    /// `My Projects` is two, so the only way to press such a directory is to have the run list it and
    /// hand the entries over as data rather than as text to be re-scanned.
    ///
    /// What is asserted here is the contract the page's listing depends on: the entries carry the
    /// **full path the process built** (the page never joins a name onto a directory -- separators are
    /// the process's business, and a name with a space or a backslash is a name), the parent so the
    /// panel can go up, and the same order the `list` tool prints -- asserted against that tool rather
    /// than against a fixture, because the whole guarantee is that a person and a model reading this
    /// directory are told the same thing.
    #[tokio::test]
    async fn a_directory_is_read_as_a_listing_of_what_is_in_it() {
        use crate::tools::Tool as _;
        let dir = scratch("dir-route");
        std::fs::create_dir_all(dir.join("My Projects")).expect("scratch directory");
        written(&dir, "notes.txt", "one\n");
        written(&dir, "My Projects/inside.txt", "two\n");

        let response = ask(&ours(&format!("/dir?path={}", encoded(&dir.display().to_string()))), &state());
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.content_type, "application/json; charset=utf-8");

        let json: serde_json::Value = serde_json::from_str(&response.body).expect("JSON");
        assert_eq!(json["path"], dir.display().to_string());
        let parent = json["parent"].as_str().expect("a parent to go up to");
        assert_eq!(std::path::Path::new(parent), dir.parent().expect("a scratch parent"));

        let names: Vec<&str> = json["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| e["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names, ["My Projects", "notes.txt"], "{}", response.body);

        let inside = &json["entries"][0];
        assert_eq!(inside["dir"], true);
        assert_eq!(
            inside["path"],
            dir.join("My Projects").display().to_string(),
            "the path is joined by the process, so a name with a space is one name"
        );
        let file = &json["entries"][1];
        assert_eq!(file["dir"], false);
        assert_eq!(file["size"], 4);

        // The same listing the tool prints, in the same order: one directory, two doors.
        let printed = crate::tools::ListTool
            .call(&serde_json::json!({ "path": dir.display().to_string() }))
            .await
            .expect("list");
        let lines: Vec<&str> = printed.lines().collect();
        assert_eq!(lines.len(), 2, "{printed:?}");
        assert!(lines[0].starts_with("My Projects/"), "{printed:?}");
        assert!(lines[1].starts_with("notes.txt  (4 bytes)"), "{printed:?}");

        // A file is not a directory, and neither is nothing at all.
        let file = ask(&ours(&format!("/dir?path={}", encoded("notes.txt"))), &working_in(&dir));
        assert_eq!(file.status, 400, "{}", file.body);
        assert!(file.body.contains("not a directory"), "{}", file.body);
        let missing = ask(&ours(&format!("/dir?path={}", encoded("nowhere"))), &working_in(&dir));
        assert_eq!(missing.status, 404, "{}", missing.body);
        let unsaid = ask(&ours("/dir"), &working_in(&dir));
        assert_eq!(unsaid.status, 400, "{}", unsaid.body);
        assert!(unsaid.body.contains("path"), "{}", unsaid.body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A bare path with a space in it is resolved **by the run**, because the run is the only reader
    /// that can `stat`.
    ///
    /// Reported directly, 2026-09-23: *`C:\Users\zhangzhuo\My Documents` still cannot be recognised* --
    /// a real directory on this machine, and two tokens to anything that reads a line as text. No rule
    /// about the *text* can tell `C:\work\My Notes` from a path followed by a word; the process can,
    /// by asking the filesystem, and this route is that question. Longest prefix first, word by word,
    /// with trailing punctuation trimmed at each step -- so `read <path>, and then` names the file and
    /// not the comma, and a sentence that happens to follow a path is not part of it.
    ///
    /// `used` counts **UTF-16 code units**, which is what a JavaScript string index is; the page cuts
    /// the line at the path it found with `slice(0, used)`. A directory named in Chinese is what that
    /// counting is for, and it is why this is asserted with one.
    #[test]
    fn the_run_says_where_a_path_in_a_line_ends() {
        let dir = scratch("resolve");
        std::fs::create_dir_all(dir.join("My Projects")).expect("scratch directory");
        std::fs::create_dir_all(dir.join("\u{8d44}\u{6599} \u{5939}")).expect("scratch CJK directory");
        written(&dir, "My Projects/inside.txt", "two\n");
        written(&dir, "notes file.txt", "three\n");
        let state = working_in(&dir);
        let base = dir.display().to_string();
        let units = |s: &str| -> usize { s.chars().map(char::len_utf16).sum() };

        // The whole name, which is what a token reader cannot see: two words, one directory. The words
        // after it belong to the sentence, and the answer says so by counting only what it used.
        let spaced = format!("{base}\\My Projects is where it goes");
        let found = ask(&ours(&format!("/resolve?text={}", encoded(&spaced))), &state);
        assert_eq!(found.status, 200, "{}", found.body);
        assert_eq!(found.content_type, "application/json; charset=utf-8");
        let json: serde_json::Value = serde_json::from_str(&found.body).expect("JSON");
        assert_eq!(json["path"], dir.join("My Projects").display().to_string(), "{}", found.body);
        assert_eq!(json["used"], units(&format!("{base}\\My Projects")), "{}", found.body);

        // A file with a space, followed by a comma: the punctuation is the sentence's, not the name's.
        // The text handed over begins at the path -- the page sends the line *from there*, because a
        // token reader is the thing that cannot see where the name starts either.
        let sentence = format!("{base}\\notes file.txt, and then carry on");
        let file = ask(&ours(&format!("/resolve?text={}", encoded(&sentence))), &state);
        assert_eq!(file.status, 200, "{}", file.body);
        let json: serde_json::Value = serde_json::from_str(&file.body).expect("JSON");
        assert_eq!(json["path"], dir.join("notes file.txt").display().to_string(), "{}", file.body);
        assert_eq!(json["used"], units(&format!("{base}\\notes file.txt")), "{}", file.body);

        // Non-ASCII names are counted in UTF-16 units, because the reader is a JavaScript string.
        let cjk = format!("{base}\\\u{8d44}\u{6599} \u{5939} and then");
        let counted = ask(&ours(&format!("/resolve?text={}", encoded(&cjk))), &state);
        assert_eq!(counted.status, 200, "{}", counted.body);
        let json: serde_json::Value = serde_json::from_str(&counted.body).expect("JSON");
        assert_eq!(json["path"], dir.join("\u{8d44}\u{6599} \u{5939}").display().to_string(), "{}", counted.body);
        assert_eq!(json["used"], units(&format!("{base}\\\u{8d44}\u{6599} \u{5939}")), "{}", counted.body);

        // Nothing in it names anything: a refusal, not an empty answer -- and not a 500.
        let nothing = ask(&ours(&format!("/resolve?text={}", encoded("and then some words"))), &state);
        assert_eq!(nothing.status, 404, "{}", nothing.body);
        assert!(nothing.body.contains("names a path"), "{}", nothing.body);

        let unsaid = ask(&ours("/resolve"), &state);
        assert_eq!(unsaid.status, 400, "{}", unsaid.body);
        assert!(unsaid.body.contains("text"), "{}", unsaid.body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file too big to show is **cut and says so**. A preview that quietly showed the first
    /// 512 KB of a 40 MB log would be this page lying about a file, which is the one thing it is
    /// built not to do.
    #[test]
    fn a_file_too_big_to_preview_is_cut_on_a_character_boundary_and_counted() {
        let dir = scratch("file-big");
        // One multi-byte character straddling the cap, so the cut has to move back to a boundary
        // rather than serve half a character -- which the page would render as a replacement glyph.
        let body = format!("{}\u{4e16}\u{754c}", "x".repeat(FILE_PREVIEW_MAX - 1));
        written(&dir, "big.log", &body);

        let response = ask(&ours(&format!("/file?path={}", encoded("big.log"))), &working_in(&dir));
        assert_eq!(response.status, 200, "{}", response.body);
        assert!(
            response.body.len() < FILE_PREVIEW_MAX,
            "the cut is on a boundary, so it lands below the cap: {}",
            response.body.len()
        );
        assert!(response.body.starts_with("xxx"), "the body is the file's own text");
        assert_eq!(header(&response, "X-Flint-Cut"), Some(response.body.len().to_string().as_str()));
        assert_eq!(header(&response, "X-Flint-Size"), Some(body.len().to_string().as_str()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §4.2 again, for the one route that reads whatever it is pointed at: no token, no file.
    #[test]
    fn the_file_route_needs_the_token_like_every_other() {
        let dir = scratch("file-token");
        written(&dir, "secret.txt", "not for a page without the token\n");
        let state = working_in(&dir);

        let none = ask(
            &format!("GET /file?path={} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", encoded("secret.txt")),
            &state,
        );
        assert_eq!(none.status, 403);
        assert!(!none.body.contains("not for a page"), "{}", none.body);

        // And the token in the *query string* is still accepted on `/` alone.
        let in_query = ask(
            &format!(
                "GET /file?path={}&token={} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n",
                encoded("secret.txt"),
                state.token
            ),
            &state,
        );
        assert_eq!(in_query.status, 403);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ask `/image`, which answers with bytes rather than text. A helper of its own because
    /// [`ask`] unwraps a `Response` and this route has none.
    fn ask_image(target: &str, state: &State) -> std::result::Result<Raw, Response> {
        let request = Request::parse(&ours(target)).unwrap_or_else(|| panic!("fixture must parse: {target:?}"));
        match respond(&request, "", state) {
            Answer::Raw(raw) => Ok(raw),
            Answer::Once(response) => Err(response),
            Answer::Events { .. } => panic!("`/image` does not stream"),
        }
    }

    /// A picture is served as the bytes it is, with the type its own first bytes declare.
    ///
    /// The claim has three parts and each is a thing that could be got wrong: the body is the file
    /// (not a description of it, not a truncation), the type is the sniffed one, and the four
    /// security headers travel with a bytes body exactly as they do with a text one -- which is the
    /// half a second response shape could quietly have lost.
    #[test]
    fn an_image_is_served_as_its_own_bytes_with_the_type_they_declare() {
        let dir = scratch("image-serve");
        // A PNG signature plus filler: the route's question is what the first bytes say, and the
        // formats themselves are held by `image_kind_knows_the_formats_a_browser_draws` below.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x01, 0x02]);
        std::fs::write(dir.join("cell.png"), &png).expect("scratch png");
        let state = working_in(&dir);

        let raw = ask_image(&format!("/image?path={}", encoded("cell.png")), &state).expect("a picture");
        assert_eq!(raw.content_type, "image/png");
        assert_eq!(raw.body, png, "the body is the file's own bytes");
        let rendered = String::from_utf8_lossy(&raw.render()).to_string();
        assert!(rendered.contains(&format!("Content-Length: {}", png.len())), "{rendered}");
        for header in ["Content-Security-Policy", "X-Content-Type-Options", "Cache-Control", "Referrer-Policy"] {
            assert!(rendered.contains(header), "the {header} header travels with bytes too: {rendered}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The type is what the file **is**, not what it is called: a JPEG named `.png` is a JPEG, and a
    /// text file named `.png` is refused rather than sent to a decoder.
    #[test]
    fn an_images_type_is_its_bytes_and_not_its_name() {
        let dir = scratch("image-names");
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0];
        jpeg.extend_from_slice(b"JFIF filler");
        std::fs::write(dir.join("photo.png"), &jpeg).expect("scratch jpeg");
        written(&dir, "notes.png", "this is text, whatever the name says\n");
        let state = working_in(&dir);

        let misnamed = ask_image(&format!("/image?path={}", encoded("photo.png")), &state).expect("a picture");
        assert_eq!(misnamed.content_type, "image/jpeg", "the name is not evidence");

        let text = ask_image(&format!("/image?path={}", encoded("notes.png")), &state)
            .expect_err("text is not a picture");
        assert_eq!(text.status, 400, "{}", text.body);
        assert!(text.body.contains("not a picture"), "{}", text.body);
        assert!(text.body.contains("png, jpeg, gif"), "the sentence names what it would draw: {}", text.body);

        // And the same text is what `/file` serves, which is the page's fallback: the two routes
        // answer one press between them.
        let as_text = ask(&ours(&format!("/file?path={}", encoded("notes.png"))), &state);
        assert_eq!(as_text.status, 200, "{}", as_text.body);
        assert!(as_text.body.contains("this is text"), "{}", as_text.body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every way `/image` can fail has a sentence, for the reason `/file`'s refusals do.
    #[test]
    fn a_missing_directory_and_oversized_image_each_say_what_they_are() {
        let dir = scratch("image-refusals");
        std::fs::create_dir_all(dir.join("sub")).expect("scratch dir");
        let state = working_in(&dir);

        let missing = ask_image(&format!("/image?path={}", encoded("nope.png")), &state).expect_err("nothing there");
        assert_eq!(missing.status, 404);
        assert!(missing.body.contains("nope.png"), "{}", missing.body);

        let directory = ask_image(&format!("/image?path={}", encoded("sub")), &state).expect_err("a directory");
        assert_eq!(directory.status, 400, "{}", directory.body);
        assert!(directory.body.contains("directory"), "{}", directory.body);

        // Sparse rather than written out: `set_len` makes a file that *is* 24 MB without spending the
        // seconds and the disk to fill it, and the route only ever reads its first 24 MB.
        let huge = std::fs::File::create(dir.join("huge.png")).expect("scratch huge");
        huge.set_len(IMAGE_MAX + 1).expect("sparse length");
        drop(huge);
        let oversized = ask_image(&format!("/image?path={}", encoded("huge.png")), &state).expect_err("too big");
        assert_eq!(oversized.status, 400, "{}", oversized.body);
        assert!(oversized.body.contains("too big"), "{}", oversized.body);
        assert!(oversized.body.contains("open it where it lives"), "{}", oversized.body);

        // Asked with no path, and with an empty one: questions answered, not missing routes.
        let unsaid = ask_image("/image", &state).expect_err("no path");
        assert_eq!(unsaid.status, 400);
        assert!(unsaid.body.contains("path"), "{}", unsaid.body);
        let empty = ask_image("/image?path=", &state).expect_err("empty path");
        assert_eq!(empty.status, 400);
        assert!(empty.body.contains("empty"), "{}", empty.body);

        // No token, no picture: the shared guard is the only one, and it is in front of this route
        // too -- asserted here rather than assumed, because a bytes response is a new shape and the
        // one thing that shape could have bypassed is the check that runs before the match.
        let untokened = respond(
            &Request::parse(&format!(
                "GET /image?path={} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n",
                encoded("huge.png")
            ))
            .expect("fixture parses"),
            "",
            &state,
        )
        .once();
        assert_eq!(untokened.status, 403, "{}", untokened.body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sniffing table, one real signature at a time, including the four cases that are *not* a
    /// picture: text, an empty file, a RIFF container that is sound rather than a picture, and an
    /// ISO media file whose brand is a video's.
    #[test]
    fn image_kind_knows_the_formats_a_browser_draws() {
        let kinds: Vec<(&str, Vec<u8>)> = vec![
            ("image/png", b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0d".to_vec()),
            ("image/jpeg", vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x18]),
            ("image/gif", b"GIF87a\x01\x00\x01\x00".to_vec()),
            ("image/gif", b"GIF89a\x01\x00\x01\x00".to_vec()),
            ("image/webp", b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec()),
            ("image/bmp", b"BM\x36\x00\x00\x00".to_vec()),
            ("image/x-icon", vec![0x00, 0x00, 0x01, 0x00, 0x01, 0x00]),
            ("image/x-icon", vec![0x00, 0x00, 0x02, 0x00, 0x01, 0x00]),
            ("image/tiff", b"II\x2a\x00\x08\x00\x00\x00".to_vec()),
            ("image/tiff", b"MM\x00\x2a\x00\x00\x00\x08".to_vec()),
            ("image/avif", b"\x00\x00\x00\x20ftypavif".to_vec()),
            ("image/heic", b"\x00\x00\x00\x18ftypheic".to_vec()),
            ("image/heic", b"\x00\x00\x00\x18ftypmif1".to_vec()),
            ("image/svg+xml", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec()),
            (
                "image/svg+xml",
                b"<?xml version=\"1.0\"?>\n<!-- a comment -->\n<svg width=\"1\"/>".to_vec(),
            ),
        ];
        for (kind, bytes) in kinds {
            assert_eq!(image_kind(&bytes), Some(kind), "for {kind}");
        }

        let not_pictures: Vec<(&str, Vec<u8>)> = vec![
            ("plain text", b"hello, this is a note\n".to_vec()),
            ("empty", Vec::new()),
            ("a RIFF that is sound", b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec()),
            ("a video", b"\x00\x00\x00\x18ftypmp42".to_vec()),
            ("xml that is not svg", b"<?xml version=\"1.0\"?><rss/>".to_vec()),
            ("a short file", b"\x89PNG".to_vec()),
        ];
        for (what, bytes) in not_pictures {
            assert_eq!(image_kind(&bytes), None, "{what} is not a picture");
        }
    }

    /// A path is never looked up on disk, so there is no traversal to get wrong (§6).
    ///
    /// `/file` is the route that *does* read a path, and it is not an exception to this: it takes
    /// its path as a **parameter**, never as a route, so a request for `/../../etc/passwd` is still
    /// an unknown route. What the parameter may name is §12's subject.
    #[test]
    fn an_unknown_route_is_a_404_and_the_method_is_checked() {
        let state = state();
        assert_eq!(ask(&ours("/../../etc/passwd"), &state).status, 404);
        assert_eq!(ask(&ours("/nope"), &state).status, 404);
        // `/events` is a route now, and the one that answers with a stream instead of a
        // single response -- which is why `Answer` has two shapes.
        let parsed = Request::parse(&ours("/events")).expect("fixture parses");
        assert!(matches!(respond(&parsed, "", &state), Answer::Events { .. }));
        assert_eq!(
            ask(
                &format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nX-Flint-Token: {}\r\n\r\n", state.token),
                &state
            )
            .status,
            405
        );
    }

    /// A malformed request is a 400 rather than a panic or a dropped connection.
    #[test]
    fn rubbish_is_answered_rather_than_fatal() {
        assert!(Request::parse("").is_none());
        assert!(Request::parse("GET\r\n\r\n").is_none());
        assert!(Request::parse("GET / HTTP/2\r\n\r\n").is_none(), "HTTP/2 is framed differently and is not supported");
        assert!(Request::parse("GET http://elsewhere/ HTTP/1.1\r\n\r\n").is_none(), "absolute-form targets are not this program's business");
    }

    /// The headers a browser is told, all of them load-bearing.
    #[test]
    fn the_response_carries_the_headers_that_keep_it_local() {
        let raw = String::from_utf8(ask(&ours("/"), &state()).render()).expect("utf-8");
        assert!(raw.starts_with("HTTP/1.1 200 OK\r\n"), "{raw}");
        assert!(raw.contains("\r\nConnection: close\r\n"));
        assert!(raw.contains("\r\nCache-Control: no-store\r\n"));
        assert!(raw.contains("\r\nX-Content-Type-Options: nosniff\r\n"));
        // The token is in the URL, so a `Referer` is the one way it could leave.
        assert!(raw.contains("\r\nReferrer-Policy: no-referrer\r\n"));
        // A second line of defence for "loads nothing external": even if a future edit
        // introduced a `<script src`, the browser would refuse to fetch it.
        assert!(
            raw.contains("default-src 'none'") && raw.contains("connect-src 'self'"),
            "the policy must block everything except our own origin: {raw}"
        );
        assert!(!raw.contains("Access-Control-Allow-Origin"), "this is not a public API");
    }
}

/// The socket, driven with real bytes.
///
/// The pure tests above say what the rules are; these say that a connection reaches them.
/// A router that is perfect and never wired to a socket passes the first and fails these.
#[cfg(test)]
mod socket_tests {
    use super::*;

    /// A guard nobody has set: a window bound by a caller that is not a run. See `Window::open`.
    fn unguarded() -> Arc<std::sync::atomic::AtomicBool> {
        Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    /// A raw HTTP/1.1 GET, read to the end. `host` of `None` omits the header entirely.
    async fn get(port: u16, target: &str, host: Option<&str>, token: Option<&str>) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect");
        let mut request = format!("GET {target} HTTP/1.1\r\n");
        if let Some(host) = host {
            request.push_str(&format!("Host: {host}\r\n"));
        }
        if let Some(token) = token {
            request.push_str(&format!("X-Flint-Token: {token}\r\n"));
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8_lossy(&response).to_string()
    }

    /// Send raw bytes and read whatever comes back.
    ///
    /// Raw, because these two cases are about the *framing* of a request rather than about what
    /// it asks for: a body the reader has to assemble across reads, and a body that never
    /// finishes arriving. A helper that built a well-formed request could not express either.
    async fn send(port: u16, request: &[u8]) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect");
        stream.write_all(request).await.expect("write");
        // The write half is closed before reading, and that is not tidiness: a request whose
        // `Content-Length` promises more than it sends is exactly one of the cases below, and
        // without this the server waits for a body that is never coming while the test waits
        // for a response that cannot be sent.
        stream.shutdown().await.expect("close the write half");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8_lossy(&response).to_string()
    }

    /// A body split across reads is still one message.
    ///
    /// The reader loop exists because a TCP read ends where it ends, not where the message
    /// does. A short body with a longer `Content-Length` also arrives here -- the loop keeps
    /// reading -- and the message must not be truncated to the first chunk.
    #[tokio::test]
    async fn a_message_body_is_assembled_across_reads() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<FromPage>();
        let window = Window::open_following(
            0,
            Arc::new(Mutex::new(None)),
            None,
            Some(tx),
            std::env::temp_dir(),
            unguarded(),
        )
        .await
        .expect("bind");

        // A space at the end of every chunk, so a reader that stopped early would produce
        // something visibly unfinished rather than something plausible.
        let text = "x".repeat(9000);
        let json = serde_json::json!({ "text": text }).to_string();
        let head = format!(
            "POST /message HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Flint-Token: {}\r\n\
             Content-Length: {}\r\n\r\n",
            window.port(),
            window.token,
            json.len()
        );

        let port = window.port();
        let expected = text.clone();
        let writer = tokio::spawn(async move {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .expect("connect");
            stream.write_all(head.as_bytes()).await.expect("head");
            for piece in json.as_bytes().chunks(97) {
                stream.write_all(piece).await.expect("piece");
                tokio::task::yield_now().await;
            }
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.expect("read");
            String::from_utf8_lossy(&response).to_string()
        });

        let response = writer.await.expect("the writer task");
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
        assert_eq!(
            rx.recv().await.expect("a message must arrive"),
            FromPage::Line(expected),
            "the body was not assembled whole"
        );
    }

    /// A body that stops early is refused rather than half-read.
    ///
    /// The alternative -- take what arrived -- turns a truncated message into a message, and
    /// there is no way for anything downstream to tell the difference.
    #[tokio::test]
    async fn a_message_that_stops_arriving_is_refused() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<FromPage>();
        let window = Window::open_following(0, Arc::new(Mutex::new(None)), None, Some(tx), std::env::temp_dir(), unguarded())
            .await
            .expect("bind");

        let head = format!(
            "POST /message HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Flint-Token: {}\r\n\
             Content-Length: 500\r\n\r\n",
            window.port(),
            window.token
        );
        // The connection is closed after eleven bytes of a promised five hundred.
        let mut request = head.into_bytes();
        request.extend_from_slice(br#"{"text":"hi"#);
        let response = send(window.port(), &request).await;

        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(rx.try_recv().is_err(), "a truncated body must not be queued");
    }

    /// A body over the ceiling is refused without being read.
    #[tokio::test]
    async fn a_message_over_the_ceiling_is_refused() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<FromPage>();
        let window = Window::open_following(0, Arc::new(Mutex::new(None)), None, Some(tx), std::env::temp_dir(), unguarded())
            .await
            .expect("bind");

        let head = format!(
            "POST /message HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Flint-Token: {}\r\n\
             Content-Length: {}\r\n\r\n",
            window.port(),
            window.token,
            MAX_BODY + 1
        );
        let response = send(window.port(), head.as_bytes()).await;

        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(rx.try_recv().is_err());
    }

    /// The URL is the contract: whatever it says must be what the listener accepts.
    #[tokio::test]
    async fn the_url_that_is_printed_is_the_one_that_opens_the_window() {
        let window = Window::open(0, None, None).await.expect("bind");
        let url = window.url();

        // Taken apart rather than rebuilt, so the test fails if the URL stops naming the
        // address the listener actually bound.
        let prefix = format!("http://127.0.0.1:{}", window.port());
        let target = url
            .strip_prefix(&prefix)
            .unwrap_or_else(|| panic!("the URL must start with the bound address, got {url}"));

        let response = get(
            window.port(),
            target,
            Some(&format!("127.0.0.1:{}", window.port())),
            None,
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert!(
            response.ends_with(VIEW_HTML),
            "the body over the socket must be the embedded page, byte for byte"
        );
    }

    #[tokio::test]
    async fn a_connection_without_the_token_gets_nothing_over_the_wire() {
        let window = Window::open(0, None, None).await.expect("bind");
        let host = format!("127.0.0.1:{}", window.port());
        let response = get(window.port(), "/", Some(&host), None).await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!response.contains("<!doctype html>"), "the page leaked without a token");
    }

    /// Each run gets its own port and its own token, which is what makes either mean
    /// anything: a token that repeated between runs would be a password written down.
    #[tokio::test]
    async fn every_run_gets_its_own_port_and_its_own_token() {
        let first = Window::open(0, None, None).await.expect("bind");
        let second = Window::open(0, None, None).await.expect("bind");
        assert_ne!(first.port(), second.port(), "two listeners cannot share a port");
        assert_ne!(first.token, second.token, "two runs must not share a token");
        assert_eq!(first.token.len(), 32, "128 bits, as hex");

        // And the first window's token really does not open the second.
        let host = format!("127.0.0.1:{}", second.port());
        let response = get(second.port(), "/", Some(&host), Some(&first.token)).await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }

    /// `/new` and `/resume` move which file `/session` serves, with the window still open.
    ///
    /// Asserted through the route rather than through the field, because the field being
    /// right is not the guarantee. The guarantee is that a browser re-reading after the
    /// `reset` gets the conversation the terminal is in.
    #[tokio::test]
    async fn the_served_conversation_follows_the_run_to_a_new_file() {
        let tag = std::process::id();
        let first = std::env::temp_dir().join(format!("flint-web-follow-a-{tag}.jsonl"));
        let second = std::env::temp_dir().join(format!("flint-web-follow-b-{tag}.jsonl"));
        std::fs::write(&first, "{\"type\":\"meta\",\"id\":\"first\"}\n").expect("write");
        std::fs::write(&second, "{\"type\":\"meta\",\"id\":\"second\"}\n").expect("write");

        let mut viewer = Viewer::asked(0, Some(first.clone()), None, std::env::temp_dir(), false);
        let url = viewer.open(0).await.expect("bind");
        let port: u16 = url
            .trim_start_matches("http://127.0.0.1:")
            .split('/')
            .next()
            .expect("port")
            .parse()
            .expect("the URL carries a port");
        let token = url.split("token=").nth(1).expect("token").to_string();
        let host = format!("127.0.0.1:{port}");

        let before = get(port, "/session", Some(&host), Some(&token)).await;
        assert!(before.contains("\"id\":\"first\""), "{before}");

        viewer.follow(Some(second.clone()));
        let after = get(port, "/session", Some(&host), Some(&token)).await;
        assert!(
            after.contains("\"id\":\"second\""),
            "the window is still serving the conversation that was left: {after:?}"
        );
        assert!(
            !after.contains("\"id\":\"first\""),
            "the old conversation is still being served: {after:?}"
        );

        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
    }
}

/// The live feed, driven directly: no socket, no timing.
#[cfg(test)]
mod live_tests {
    use super::*;

    /// The page's log is appended, single-lined and readable by a person afterwards.
    ///
    /// All three are the point of it: appended because two entries must both survive, single-lined
    /// because a page that sends a newline must not be able to forge a second entry, and readable
    /// because the entire reason it exists is that somebody reads it later on a machine that is not
    /// the one it ran on.
    #[test]
    fn the_pages_log_is_appended_one_plain_line_at_a_time() {
        let dir = std::env::temp_dir().join(format!("flint-weblog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("web.log");

        assert_eq!(accept_log_in(&path, b"first").status, 200);
        assert_eq!(accept_log_in(&path, b"second\r\nthird").status, 200);

        let text = std::fs::read_to_string(&path).expect("the log exists");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "two entries, not three: {text:?}");
        assert!(lines[0].ends_with(" first"), "{:?}", lines[0]);
        assert!(
            lines[1].ends_with(" second third"),
            "the newline inside an entry was flattened: {:?}",
            lines[1]
        );
        assert!(
            lines[0].split(' ').next().unwrap().parse::<u64>().is_ok(),
            "an entry is stamped: {:?}",
            lines[0]
        );

        assert_eq!(
            accept_log_in(&path, b"   \n  ").status,
            400,
            "an empty line is not a record"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `/new` and `/resume` have to reach a browser that is already connected.
    ///
    /// Two mechanisms, and they cover different clients. One that is *connected* is told by a
    /// frame, because no cursor difference can reach it: it is already current, so there is
    /// nothing for it to notice. One that reconnects is told by its cursor going too old to
    /// satisfy, which is why the ring is emptied rather than left in place -- the frames in
    /// it belong to the conversation being left, and a page that replayed them on top of the
    /// new session file would show two conversations spliced together.
    #[test]
    fn a_change_of_conversation_is_a_named_reset_and_drops_the_old_frames() {
        let live = Live::new();
        live.event(&Event::Text("from the old conversation".to_string()));
        let before = live.newest_seq();

        let (mut rx, _) = live.follow(Some(before), None);
        live.restart();

        let frame = rx.try_recv().expect("the connected client must be told");
        let rendered = frame.render();
        assert!(
            rendered.contains("event: reset"),
            "the frame must be named, or the page applies it as a run event: {rendered:?}"
        );
        assert!(
            !rendered.contains("from the old conversation"),
            "the reset must not carry the conversation it replaces: {rendered:?}"
        );

        // A client that reconnects *after* the switch is not handed a frame at all: the ring was
        // emptied, so the cursor it holds cannot be placed in it, and that is a reload -- the
        // same outcome by the same route, because `/session` now serves the new conversation and
        // the page rebuilds from it. The distinction the frame numbering used to draw here (an
        // id below the oldest, an id above the newest) is not one a *position* can draw: what a
        // position can say is whether the ring still holds everything after it, and after a
        // switch it does not claim to.
        match live.follow(Some(before), None).1 {
            Catch::Reset => {}
            Catch::Missed(frames) => panic!(
                "the ring was emptied of that cursor, but {} frame(s) were replayed",
                frames.len()
            ),
        }

        // Further back than the ring can serve -- which is now every cursor from before the
        // restart -- and the answer is to reload rather than to guess.
        match live.follow(Some(before.saturating_sub(5)), None).1 {
            Catch::Reset => {}
            Catch::Missed(frames) => panic!(
                "a cursor the ring cannot serve was answered with {} frame(s)",
                frames.len()
            ),
        }
    }

    /// The status is state rather than history, so dropping the ring is not enough alone.
    #[test]
    fn a_change_of_conversation_forgets_what_the_old_one_was_waiting_for() {
        let live = Live::new();
        live.event(&Event::Status {
            text: "waiting for the model".to_string(),
            restarted: true,
        });
        assert_eq!(live.current_status(), "waiting for the model");
        live.restart();
        assert_eq!(
            live.current_status(),
            "",
            "a new conversation must not open showing the old one's status"
        );
    }

    /// `/web` twice must not leave two listeners running.
    ///
    /// The failure it prevents is quiet: a second bind succeeds on a second port, the second
    /// URL is printed, and the browser a person already has open is showing a different
    /// socket from the one the terminal just told them about. Nothing errors, and the two
    /// disagree about which run they are watching.
    #[tokio::test]
    async fn asking_for_the_view_twice_opens_one_listener() {
        let mut viewer = Viewer::asked(0, None, None, std::env::temp_dir(), false);
        let first = viewer.open(0).await.expect("bind");
        let second = viewer.open(0).await.expect("bind again");
        assert_eq!(first, second, "the second ask must report where it already is");
    }

    /// And a viewer that was only *asked* for is not broken -- it is unbound.
    #[test]
    fn a_viewer_that_has_not_been_opened_has_no_window() {
        let viewer = Viewer::asked(0, None, None, std::env::temp_dir(), false);
        assert!(viewer.window.is_none());
        // The feed exists anyway, which is the point of the split: a turn that starts while
        // the socket is still being bound does not lose its frames.
        viewer.live().event(&Event::Text("while binding".to_string()));
        assert_eq!(viewer.live().newest_seq(), 1);
    }

    #[test]
    fn a_client_that_has_just_loaded_is_told_nothing_it_missed() {
        let live = Live::new();
        live.event(&Event::Text("hello".to_string()));
        // `None` means "I have read /session and I am current", which is what the page
        // sends on its first connection.
        let (_, catch) = live.follow(None, None);
        match catch {
            Catch::Missed(frames) => assert!(frames.is_empty(), "a fresh client is current"),
            Catch::Reset => panic!("a fresh client must not be reset"),
        }
    }

    #[test]
    fn a_reconnecting_client_gets_exactly_what_it_missed() {
        let live = Live::new();
        live.event(&Event::Text("one".to_string()));
        live.event(&Event::Text("two".to_string()));
        let (_, catch) = live.follow(Some(1), None);
        match catch {
            Catch::Missed(frames) => {
                assert_eq!(frames.len(), 1, "only the frame after the one it saw");
                assert!(frames[0].line.contains("two"), "{}", frames[0].line);
            }
            Catch::Reset => panic!("frame 2 is still in the ring"),
        }
    }

    /// Frames older than the buffer are not silently skipped.
    ///
    /// This is the whole reason `Catch` has two shapes. A client that is quietly sent the
    /// frames we happen to still have would render a transcript with a hole in it and no
    /// sign that anything was missing.
    #[test]
    fn a_client_that_fell_too_far_behind_is_told_to_reload() {
        let live = Live::new();
        for n in 0..(RECENT_FRAMES + 10) {
            live.event(&Event::Text(format!("line {n}")));
        }
        match live.follow(Some(1), None).1 {
            Catch::Reset => {}
            Catch::Missed(frames) => panic!("frame 1 is long gone, but {} were replayed", frames.len()),
        }
        // And the id just before the oldest kept one still works: the boundary is off by
        // one in the direction that loses nothing.
        let oldest = live.recent.lock().unwrap().front().map(|f| f.seq).expect("frames");
        match live.follow(Some(oldest), None).1 {
            Catch::Missed(frames) => assert_eq!(frames.len(), RECENT_FRAMES - 1),
            Catch::Reset => panic!("the oldest kept frame is still replayable"),
        }
    }

    /// An id from a previous process -- or a made-up one -- is not trusted.
    #[test]
    fn an_id_ahead_of_anything_we_sent_is_told_to_reload() {
        let live = Live::new();
        live.event(&Event::Text("one".to_string()));
        assert!(matches!(live.follow(Some(9999), None).1, Catch::Reset));
    }

    /// A session file under a scratch directory, and the cursor just past its first entry.
    ///
    /// The file is the fixture and the position is the question, in every test below: what a
    /// cursor means is the whole of this feature.
    fn a_session_of_two_entries(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, u64) {
        let dir = std::env::temp_dir().join(format!("flint-web-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let path = dir.join("session.jsonl");
        let meta = "{\"type\":\"meta\",\"id\":\"abc123\",\"version\":1,\"cwd\":\"/tmp\"}\n";
        let asked =
            "{\"type\":\"chat\",\"message\":{\"role\":\"user\",\"content\":\"the first question\"}}\n";
        let answered = "{\"type\":\"chat\",\"message\":{\"role\":\"assistant\",\"content\":\"the first answer\"}}\n";
        std::fs::write(&path, format!("{meta}{asked}{answered}")).expect("write the session");
        let at = (meta.len() + asked.len()) as u64;
        (dir, path, at)
    }

    /// The cursor is a position in the file, so a process that did not mint it can answer it.
    ///
    /// This is what "resume after a restart" means here. A frame's `id` is the length of the
    /// session file when it was pushed, so a cursor names a position in a *file* rather than a
    /// position in one process's memory. A ring of nothing -- a process that has only just
    /// started -- cannot replay a single frame of what the client missed, and the file can:
    /// every entry the client does not have is still there, in order, whatever happened to the
    /// process in between. Before this, that request was answered with `reset`, and the page
    /// re-read the whole conversation to find out what had changed.
    #[test]
    fn a_cursor_from_another_process_is_answered_out_of_the_file() {
        let (dir, path, at) = a_session_of_two_entries("cursor");
        let live = Live::new();
        live.serving(Some(path.clone()));

        match live.follow(Some(at), Some("abc123")).1 {
            Catch::Missed(frames) => {
                assert_eq!(frames.len(), 1, "one entry was written after the cursor");
                let rendered = frames[0].render();
                assert!(
                    rendered.contains("event: file"),
                    "the record, not a delta: {rendered:?}"
                );
                assert!(rendered.contains("the first answer"), "{rendered:?}");
                assert!(
                    !rendered.contains("the first question"),
                    "and nothing the client already has: {rendered:?}"
                );
                assert!(
                    rendered.contains(&format!(
                        "id: {}",
                        std::fs::read(&path).expect("read back").len()
                    )),
                    "the cursor advances to the end of the file: {rendered:?}"
                );
            }
            Catch::Reset => panic!("a restarted process must answer from the file, not reload"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ring is asked first, and it is the only source that has the deltas.
    ///
    /// A page that dropped for a second mid-answer must not be rebuilt: the part of the answer
    /// so far is in no file yet, so the frames are replayed rather than the file re-read.
    #[test]
    fn a_cursor_the_ring_can_cover_is_answered_with_frames_and_not_file_lines() {
        let (dir, path, at) = a_session_of_two_entries("ring-first");
        // The file as the client read it: without the entry the frame below will be about, so
        // that frame's own cursor *is* the client's. This is the ordinary case -- a page that
        // loaded, then dropped while an answer was streaming -- and it is the case the ring
        // exists for.
        let mut body = std::fs::read(&path).expect("read the session");
        body.truncate(at as usize);
        std::fs::write(&path, &body).expect("write the prefix");
        let live = Live::new();
        live.serving(Some(path.clone()));
        live.event(&Event::Text("a delta at the cursor".to_string()));
        // Then an entry lands, so the next frame's cursor is past the client's.
        let mut body = std::fs::read(&path).expect("read the session");
        body.extend_from_slice(b"{\"type\":\"title\",\"name\":\"later\"}\n");
        std::fs::write(&path, &body).expect("append");
        live.event(&Event::Text("a delta after it".to_string()));

        match live.follow(Some(at), Some("abc123")).1 {
            Catch::Missed(frames) => {
                assert_eq!(frames.len(), 1, "only the frame past the cursor");
                let rendered = frames[0].render();
                assert!(rendered.contains("a delta after it"), "{rendered:?}");
                assert!(
                    !rendered.contains("event: file"),
                    "the ring has it, so the file is not consulted: {rendered:?}"
                );
            }
            Catch::Reset => panic!("the ring holds the frame the client missed"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cursor names a conversation as well as a position, and the id is the conversation.
    ///
    /// Two files have the same positions, so a position alone cannot say which conversation a
    /// client is showing. The case is not hypothetical: `/resume` moves the run to another
    /// file, and a page that is reconnecting at that moment never sees the `reset` frame that
    /// would have told it -- it would be handed lines from the middle of a conversation it is
    /// not reading.
    #[test]
    fn a_cursor_from_another_conversation_is_not_answered_from_this_file() {
        let (dir, path, at) = a_session_of_two_entries("other-conversation");
        let live = Live::new();
        live.serving(Some(path.clone()));
        assert!(
            matches!(
                live.follow(Some(at), Some("some-other-session")).1,
                Catch::Reset
            ),
            "a different conversation is a reload, not somebody else's lines"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cursor past the end of the file was not minted against this file.
    #[test]
    fn a_cursor_past_the_end_of_the_file_is_told_to_reload() {
        let (dir, path, _) = a_session_of_two_entries("past-the-end");
        let live = Live::new();
        live.serving(Some(path.clone()));
        let beyond = std::fs::read(&path).expect("read").len() as u64 + 1;
        assert!(matches!(
            live.follow(Some(beyond), Some("abc123")).1,
            Catch::Reset
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every frame carries the sequence number the client sends back.
    #[test]
    fn frames_are_numbered_from_one_and_render_as_sse() {
        let live = Live::new();
        live.event(&Event::Text("hello".to_string()));
        live.event(&Event::Text("again".to_string()));
        let recent = live.recent.lock().unwrap();
        let rendered: Vec<String> = recent.iter().map(|f| f.render()).collect();
        assert!(rendered[0].starts_with("id: 1\ndata: "), "{}", rendered[0]);
        assert!(rendered[0].ends_with("\n\n"), "a frame ends with a blank line");
        assert!(rendered[1].starts_with("id: 2\ndata: "), "{}", rendered[1]);
        assert!(rendered[0].contains("\"message.delta\""));
    }

    /// The browser's stream is the `--json` stream, not a description of it.
    #[test]
    fn the_line_on_the_wire_is_what_json_would_have_printed() {
        let live = Live::new();
        live.event(&Event::Status {
            text: "waiting for the model".to_string(),
            restarted: true,
        });
        let recent = live.recent.lock().unwrap();
        let line = recent.front().expect("a frame").line.clone();
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        assert_eq!(parsed["type"], "status");
        assert_eq!(parsed["text"], "waiting for the model");
        assert_eq!(parsed["restarted"], true);
    }

    /// A live feed with no listeners is the normal case, not an error.
    #[test]
    fn pushing_with_nobody_listening_is_fine() {
        let live = Live::new();
        for n in 0..10 {
            live.event(&Event::Text(format!("{n}")));
        }
        assert_eq!(live.recent.lock().unwrap().len(), 10);
    }
}

/// The event stream, over a real socket.
#[cfg(test)]
mod stream_tests {
    use super::*;

    /// Open `/events`, read whatever arrives, and return the raw bytes.
    ///
    /// Bounded by a timeout because the point of this route is that it *does not end*: a
    /// test that waited for the socket to close would hang until the client gave up, which
    /// is exactly what the browser does not do.
    async fn read_stream(window: &Window, query: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, window.port()))
            .await
            .expect("connect");
        let request = format!(
            "GET /events{query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Flint-Token: {}\r\n\r\n",
            window.port(),
            window.token
        );
        stream.write_all(request.as_bytes()).await.expect("write");

        let mut seen = Vec::new();
        let mut chunk = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
        loop {
            match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => seen.extend_from_slice(&chunk[..n]),
                Ok(Err(_)) => break,
            }
        }
        String::from_utf8_lossy(&seen).to_string()
    }

    #[tokio::test]
    async fn the_stream_carries_the_run_as_ndjson_frames() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        // Pushed *while* the client is connected, because that is what this route is for.
        // A client with no cursor is current by definition -- it has just read `/session` --
        // so frames from before it connected are deliberately not sent, and a test that
        // pushed first would be asserting the replay path by accident.
        let pusher = {
            let live = Arc::clone(&live);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                live.event(&Event::ToolStart {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                });
                live.event(&Event::ToolResult {
                    id: "call_1".to_string(),
                    output: "1.2.3".to_string(),
                    ok: true,
                });
            })
        };

        let raw = read_stream(&window, "").await;
        pusher.await.expect("pusher");
        assert!(raw.starts_with("HTTP/1.1 200 OK\r\n"), "{raw}");
        assert!(
            raw.contains("Content-Type: text/event-stream\r\n"),
            "the browser needs the stream type: {raw}"
        );
        assert!(
            !raw.contains("Content-Length"),
            "an endless response cannot carry a length: {raw}"
        );

        // The stream is a response like any other, and §12.2 says *every* response carries the four
        // headers. This one was written by hand beside `Response::render` and carried three: the CSP
        // line was missing, while this constant's own comment claimed to have "everything a small
        // response has, except a length". Asserted here as well as on `Response`, because the two
        // blocks drifting is the failure -- one copy is the fix, and a test on each side is what
        // notices if a third copy appears.
        for header in [
            "\r\nCache-Control: no-store\r\n",
            "\r\nX-Content-Type-Options: nosniff\r\n",
            "\r\nReferrer-Policy: no-referrer\r\n",
        ] {
            assert!(
                raw.contains(header),
                "the stream is missing {header:?}: {raw}"
            );
        }
        assert!(
            raw.contains("default-src 'none'") && raw.contains("connect-src 'self'"),
            "the stream is the one response served without a policy: {raw}"
        );

        // Every `data:` line parses as one NDJSON object -- the same assertion
        // `tests/json_output.rs` makes about `--json`, because it is the same vocabulary.
        let frames: Vec<&str> = raw
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|l| *l != "{}")
            .collect();
        assert_eq!(frames.len(), 2, "expected two frames in {raw}");
        for frame in &frames {
            let parsed: serde_json::Value =
                serde_json::from_str(frame).unwrap_or_else(|e| panic!("{frame:?} is not JSON: {e}"));
            assert!(parsed["type"].is_string(), "{frame}");
        }
        assert!(frames[0].contains("tool.started"), "{}", frames[0]);
        assert!(frames[1].contains("1.2.3"), "{}", frames[1]);

        // And the ids are the cursor a reconnect sends back.
        assert!(raw.contains("id: 1\ndata: "), "{raw}");
        assert!(raw.contains("id: 2\ndata: "), "{raw}");
    }

    #[tokio::test]
    async fn a_reconnect_continues_from_the_id_it_sends() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");
        // In the ring, but *before* this connection: the replay path this test is about.
        live.event(&Event::Text("first".to_string()));
        live.event(&Event::Text("second".to_string()));

        // The cursor, as the reconnecting client sends it.
        let raw = read_stream(&window, "?last=1").await;
        // Asserted on the *frame*, not on the text: the answer snapshot carries everything said
        // so far and is sent to every client, so `first` appears in this response whatever the
        // cursor was. The cursor is the thing under test -- frame 1 must not be replayed.
        assert!(
            !raw.contains("id: 1\ndata: "),
            "it already had frame 1: {raw}"
        );
        assert!(raw.contains("id: 2\ndata: "), "it is missing frame 2: {raw}");
        assert!(!raw.contains("event: reset"), "nothing was lost, so nothing to reload: {raw}");
    }

    /// A cursor we can no longer honour means "re-read the session", not "here is what is
    /// left" -- the difference between a transcript with a hole in it and one without.
    #[tokio::test]
    async fn an_unhonourable_cursor_asks_the_page_to_reload() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");
        for n in 0..(RECENT_FRAMES + 5) {
            live.event(&Event::Text(format!("line {n}")));
        }

        let raw = read_stream(&window, "?last=1").await;
        assert!(raw.contains("event: reset\ndata: {}\n\n"), "{raw}");
        // A named SSE event and not a line in the vocabulary: "your document is stale" is a
        // fact about the transport, and the vocabulary is deliberately closed.
        assert!(
            !raw.contains("\"type\":\"reset\""),
            "the reset must not enter the run's vocabulary: {raw}"
        );
    }

    /// The stream needs the same token as everything else, and the query string is not a
    /// second way in.
    #[tokio::test]
    async fn the_stream_is_behind_the_same_token() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        // No token at all.
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, window.port()))
            .await
            .expect("connect");
        let request = format!(
            "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            window.port()
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");

        // The token in the query string, which is accepted on `/` and nowhere else.
        let raw = read_stream_token_in_query(&window).await;
        assert!(raw.starts_with("HTTP/1.1 403"), "{raw}");
    }

    async fn read_stream_token_in_query(window: &Window) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, window.port()))
            .await
            .expect("connect");
        let request = format!(
            "GET /events?token={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            window.token,
            window.port()
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8_lossy(&response).to_string()
    }
}

/// The status a client is told when it arrives in the middle of a turn.
#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[tokio::test]
    async fn a_page_opened_mid_turn_is_told_what_is_being_waited_for() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        // A turn is already running: the status went out before this page existed, and no
        // cursor can bring it back, because there is no cursor for "now".
        live.event(&Event::Status {
            text: "no response yet — the network or the endpoint may be stuck".to_string(),
            restarted: false,
        });

        let raw = read_stream_of(&window).await;
        assert!(
            raw.contains("event: status\ndata: no response yet"),
            "a page opened mid-turn must be told what is happening: {raw}"
        );
        // The snapshot is outside the numbering: it is state, not a change in the sequence.
        assert!(
            !raw.contains("id: 1\nevent: status"),
            "the snapshot is not a numbered frame: {raw}"
        );
    }

    #[tokio::test]
    async fn a_page_opened_between_turns_is_not_given_a_stale_status() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        live.event(&Event::Status {
            text: "waiting for the model".to_string(),
            restarted: true,
        });
        live.event(&Event::Status {
            text: String::new(),
            restarted: false,
        });

        let raw = read_stream_of(&window).await;
        assert!(
            !raw.contains("event: status"),
            "the turn is over, so there is nothing to report: {raw}"
        );
    }

    /// Tidying the history in the terminal has to reach the sidebar.
    ///
    /// Reported from a real session: conversations deleted in the terminal, and the page still
    /// offering them. The reason it matters is not that the list looks wrong -- it is that the
    /// numbers *are* the ones `/resume` takes, and they are positions. A stale sidebar resumes the
    /// wrong conversation, and the person carries on talking in it.
    #[tokio::test]
    async fn a_client_is_told_when_the_list_of_conversations_changes() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        // Connected first, so this is the frame a page already watching receives rather than a
        // snapshot for one that arrives later.
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, window.port()))
            .await
            .expect("connect");
        let request = format!(
            "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Flint-Token: {}\r\n\r\n",
            window.port(),
            window.token
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        live.sessions_changed();

        let mut seen = Vec::new();
        let mut chunk = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(400);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(read)) => {
                    seen.extend_from_slice(&chunk[..read]);
                    if String::from_utf8_lossy(&seen).contains("event: sessions") {
                        break;
                    }
                }
                Ok(Err(_)) => break,
            }
        }
        let raw = String::from_utf8_lossy(&seen);
        assert!(
            raw.contains("event: sessions"),
            "the sidebar was never told its list had changed: {raw}"
        );
        // And it is *not* a reset: the transcript has not changed, so the reader's place in it
        // must not be thrown away.
        assert!(
            !raw.contains("event: reset"),
            "a changed list must not rebuild the transcript: {raw}"
        );
    }

    /// The answer already written is state, and a page arriving mid-turn cannot get it anywhere
    /// else.
    ///
    /// `/session` serves the session file, and the file gets the assistant message when the turn
    /// *ends* -- so this is the only way a page that loads, reconnects or is reset in the middle
    /// of a turn can show the part of the answer that streamed before it was listening.
    #[tokio::test]
    async fn a_page_opened_mid_answer_is_told_what_has_been_said() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        live.event(&Event::Text("the first half. ".to_string()));
        live.event(&Event::Text("the second half.".to_string()));

        let raw = read_stream_of(&window).await;
        assert!(
            raw.contains("event: answer\ndata: \"the first half. the second half.\""),
            "a page opened mid-answer must be told what has been said: {raw}"
        );
    }

    /// And between turns there is nothing to say, because the file has it.
    #[tokio::test]
    async fn a_page_opened_between_turns_is_not_given_a_stale_answer() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        live.event(&Event::Text("a finished answer".to_string()));
        // `message.completed` is what drains the accumulator the snapshot reads.
        live.event(&Event::Done);

        let raw = read_stream_of(&window).await;
        assert!(
            !raw.contains("event: answer"),
            "the turn is over and the file has the answer: {raw}"
        );
    }

    /// A stopped turn is over the same way, even though nothing ever reaches `Done`.
    ///
    /// An interrupt is a dropped future, so `message.completed` -- the thing that drains the
    /// accumulator the snapshot reads -- never happens for it. The text that had been drawn is
    /// committed to the conversation by `Agent::commit_drawn_answer`, and the feed is told the
    /// same thing. Without that the next page to open would be handed a stopped half as an answer
    /// still being written, which is the one thing a stop is supposed to end.
    #[tokio::test]
    async fn a_page_opened_after_a_stop_is_not_told_the_stopped_answer_is_still_arriving() {
        let live = Live::new();
        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");

        live.event(&Event::Text("half an article".to_string()));
        live.answer_committed();

        let raw = read_stream_of(&window).await;
        assert!(
            !raw.contains("event: answer"),
            "the stopped answer is in the file now, not in flight: {raw}"
        );
    }

    /// The state frame: sent when it changes, and kept for a page that opens afterwards.
    ///
    /// Two behaviours, and they are the two ways this can be wrong. The caller is the REPL's loop,
    /// which re-derives the state on every line it reads, so pushing regardless would rebuild the
    /// page's pickers under whoever is choosing from one -- measured on the frame, not assumed.
    /// And a page that opens *after* the state was announced has missed the frame entirely: a
    /// client with no cursor is not replayed the ring, so on connect is the only chance it gets.
    #[tokio::test]
    async fn the_state_frame_is_sent_when_it_changes_and_kept_for_a_later_page() {
        let live = Live::new();
        let first = "{\"type\":\"state\",\"model\":\"stub-model\"}";
        let second = "{\"type\":\"state\",\"model\":\"stub-other\"}";
        live.state(first.to_string());
        live.state(first.to_string());
        live.state(second.to_string());

        {
            let recent = live.recent.lock().unwrap_or_else(|e| e.into_inner());
            let sent: Vec<&str> = recent
                .iter()
                .filter(|frame| frame.name == "state")
                .map(|frame| frame.line.as_str())
                .collect();
            assert_eq!(
                sent,
                vec![first, second],
                "the same state twice is one frame, and a change is a second: {sent:?}"
            );
        }

        let window = Window::open(0, None, Some(Arc::clone(&live)))
            .await
            .expect("bind");
        let raw = read_stream_of(&window).await;
        assert_eq!(
            raw.matches("event: state").count(),
            1,
            "a page that opens late is handed the state once, and no backlog of them: {raw}"
        );
        assert!(
            raw.contains("stub-other"),
            "and what it is handed is the state in force: {raw}"
        );
    }

    async fn read_stream_of(window: &Window) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, window.port()))
            .await
            .expect("connect");
        let request = format!(
            "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Flint-Token: {}\r\n\r\n",
            window.port(),
            window.token
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut seen = Vec::new();
        let mut chunk = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(250);
        loop {
            match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => seen.extend_from_slice(&chunk[..n]),
                Ok(Err(_)) => break,
            }
        }
        String::from_utf8_lossy(&seen).to_string()
    }
}
