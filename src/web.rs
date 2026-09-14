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
    input: Option<tokio::sync::mpsc::UnboundedSender<String>>,
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

/// One frame of the event stream: its sequence number and the line it carries.
#[derive(Clone)]
struct Frame {
    seq: u64,
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
        // `id:` is what a reconnecting client sends back as `Last-Event-ID`.
        if self.name.is_empty() {
            format!("id: {}\ndata: {}\n\n", self.seq, self.line)
        } else {
            format!(
                "id: {}\nevent: {}\ndata: {}\n\n",
                self.seq, self.name, self.line
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
        })
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

    /// The sequence number of the last frame, `0` when there has not been one.
    ///
    /// A snapshot, not a promise: frames can be pushed a moment later, and a client that
    /// treats this as a cursor is safe precisely because the direction of the error is
    /// known -- anything above it is newer than the read, and anything at or below it is
    /// dropped rather than applied twice.
    fn current_seq(&self) -> u64 {
        self.next.load(Ordering::Relaxed).saturating_sub(1)
    }

    fn push(&self, line: String) {
        self.push_named("", line);
    }

    fn push_named(&self, name: &'static str, line: String) {
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        let frame = Frame { seq, name, line };
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
    /// The status goes with them. It is the one piece of state here, and it describes a turn
    /// in the conversation that is over.
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

    /// Everything a client that last saw `last` needs.
    ///
    /// The subscription is taken **before** the recent frames are read, so a frame pushed
    /// in between is delivered by the channel rather than lost in the gap. Losing one there
    /// would be silent, which is the failure this route exists to prevent.
    fn follow(&self, last: Option<u64>) -> (broadcast::Receiver<Frame>, Catch) {
        let rx = self.subscribe();
        let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let catch = match last {
            // No id: the client has just loaded, and `/session` is what it read.
            None => Catch::Missed(Vec::new()),
            Some(id) => {
                let oldest = recent.front().map(|f| f.seq);
                let newest = recent.back().map(|f| f.seq);
                match (oldest, newest) {
                    // It needs frames we have already dropped.
                    (Some(oldest), _) if id + 1 < oldest => Catch::Reset,
                    // Ahead of anything we have: a stale id, or a restarted process.
                    (_, Some(newest)) if id > newest => Catch::Reset,
                    (None, _) => Catch::Reset,
                    _ => Catch::Missed(recent.iter().filter(|f| f.seq > id).cloned().collect()),
                }
            }
        };
        (rx, catch)
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
const X_FLINT_EVENT_SEQ: &str = "X-Flint-Event-Seq";

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
        let extra: String = self
            .extra
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        let mut out = format!(
            "HTTP/1.1 {} {}\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             Cache-Control: no-store\r\n\
             X-Content-Type-Options: nosniff\r\n\
             Referrer-Policy: no-referrer\r\n\
             Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
             style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:\r\n\
             {extra}\r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len()
        );
        out.push_str(&self.body);
        out.into_bytes()
    }
}

/// What a request turned into.
///
/// Two shapes because one of them has no `Content-Length` and cannot be a `Response`: the
/// event stream ends when the client goes away. Authorisation and routing still happen in
/// one function either way -- a second place that decided who may connect would be a second
/// place to get §4 wrong.
pub enum Answer {
    /// One response, then close.
    Once(Response),
    /// The live feed, for a client that last saw `last`.
    Events { last: Option<u64> },
}

impl Answer {
    /// The response, for the routes that have exactly one. Panics on a stream, which is a
    /// programming error rather than a client's.
    ///
    /// Test-only: the server itself matches on the two shapes, because for one of them
    /// there is nothing to return.
    #[cfg(test)]
    fn once(self) -> Response {
        match self {
            Answer::Once(response) => response,
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
        // Everything since, and then everything as it happens.
        ("GET", "/events") => return Answer::Events {
            last: last_event_id(request),
        },
        // A line typed into the browser, into the same channel the keyboard feeds.
        ("POST", "/message") => accept_message(body, state),
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

/// One message from the page, into the prompt.
///
/// This is the route that makes the page a composer, and it is the only route in this file that
/// can cause something to *happen* -- the others read. What keeps that acceptable is unchanged
/// from §4: loopback only, the `Host` and `Origin` checks, and a token that the page has and
/// another origin's page does not. A cross-origin form post cannot set `X-Flint-Token`, and a
/// `fetch` that could would be refused by the `Origin` check before it got here.
///
/// Note what it does *not* do: it does not interpret the text. A line beginning `/` is a slash
/// command because the REPL says so, a line beginning `!` is a shell escape because the REPL
/// says so, and neither fact is known in this file. That is the point -- there is one place
/// that decides what a typed line means, and the browser is not a second one.
fn accept_message(body: &str, state: &State) -> Response {
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
    match input.send(text.to_string()) {
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
            // Read *after* the file, deliberately: a frame at or below this number may
            // already be in the file and one above it cannot be, which is the direction
            // that risks a duplicate rather than a hole -- and a duplicate delta shows up
            // as doubled text, while a hole shows up as nothing at all.
            let mut response = Response {
                status: 200,
                reason: "OK",
                content_type: "application/x-ndjson; charset=utf-8",
                body,
                extra: Vec::new(),
            };
            if let Some(live) = &state.live {
                response = response.with_header(X_FLINT_EVENT_SEQ, live.current_seq().to_string());
            }
            response
        }
        Err(e) => Response::text(
            500,
            "Internal Server Error",
            format!("cannot read {}: {e}\n", path.display()),
        ),
    }
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
        Window::open_following(port, Arc::new(Mutex::new(session)), live, None).await
    }

    /// The same, for a caller whose session can change under it.
    ///
    /// The REPL is that caller: `/new` and `/resume` move the run to a different file with
    /// the window still open, so the two of them share the path rather than the window
    /// taking a copy at bind time.
    pub async fn open_following(
        port: u16,
        session: Arc<Mutex<Option<std::path::PathBuf>>>,
        live: Option<Arc<Live>>,
        input: Option<tokio::sync::mpsc::UnboundedSender<String>>,
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
    /// `State::input` for why this is text and not the REPL's own input event.
    input: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl Viewer {
    /// Ask for a view of `session`. Nothing is bound and nothing is listening yet.
    pub fn asked(
        port: u16,
        session: Option<std::path::PathBuf>,
        input: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Viewer {
        Viewer {
            live: Live::new(),
            window: None,
            session: Arc::new(Mutex::new(session)),
            port,
            input,
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
        )
        .await?;
        let url = window.url();
        self.window = Some(window);
        Ok(url)
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
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = session;
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
        Answer::Events { last } => {
            stream_events(stream, state, last).await?;
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
/// the reason this route writes its own headers instead of going through `Response`.
const SSE_HEADERS: &str = "\
HTTP/1.1 200 OK\r\n\
Content-Type: text/event-stream\r\n\
Cache-Control: no-store\r\n\
Connection: close\r\n\
X-Content-Type-Options: nosniff\r\n\
Referrer-Policy: no-referrer\r\n\
\r\n";

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


/// Tell a client its document is stale, so it re-reads the session.
///
/// A *named* SSE event rather than a line in the NDJSON vocabulary. Every `data:` line on
/// this stream is a fact about the run, and "your transcript is out of date" is a fact about
/// the *transport* -- the vocabulary is deliberately closed, and this does not belong in it.
const SSE_RESET: &str = "event: reset\ndata: {}\n\n";

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
async fn stream_events(mut stream: TcpStream, state: &State, last: Option<u64>) -> Result<()> {
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
    let (mut rx, catch) = live.follow(last);

    stream.write_all(SSE_HEADERS.as_bytes()).await?;
    match catch {
        Catch::Reset => {
            // What it missed is gone, so it re-reads the session instead. The file is the
            // truth and `GET /session` is already the route for it; keeping a durable log
            // of the stream would be derived state, which is the one thing this repository
            // does not keep.
            stream.write_all(SSE_RESET.as_bytes()).await?;
        }
        Catch::Missed(frames) => {
            for frame in frames {
                stream.write_all(frame.render().as_bytes()).await?;
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
    write_answer_snapshot(&mut stream, live).await?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    // The first tick of an interval is immediate; a heartbeat now would be noise on a
    // connection that has this second been handed a response.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(frame) => stream.write_all(frame.render().as_bytes()).await?,
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
                    write_answer_snapshot(&mut stream, live).await?;
                    rx = live.subscribe();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => {
                stream.write_all(b": ping\n\n").await?;
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
    fn with_input() -> (State, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
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
            "hello from the page"
        );
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
            assert_eq!(rx.try_recv().expect("queued"), text);
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

        let expected = crate::session::list(&dir).expect("list");
        assert_eq!(expected.len(), 2, "the fixture did not produce two sessions");
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

    /// A path is never looked up on disk, so there is no traversal to get wrong (§6).
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let window = Window::open_following(
            0,
            Arc::new(Mutex::new(None)),
            None,
            Some(tx),
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
            expected,
            "the body was not assembled whole"
        );
    }

    /// A body that stops early is refused rather than half-read.
    ///
    /// The alternative -- take what arrived -- turns a truncated message into a message, and
    /// there is no way for anything downstream to tell the difference.
    #[tokio::test]
    async fn a_message_that_stops_arriving_is_refused() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let window = Window::open_following(0, Arc::new(Mutex::new(None)), None, Some(tx))
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let window = Window::open_following(0, Arc::new(Mutex::new(None)), None, Some(tx))
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

        let mut viewer = Viewer::asked(0, Some(first.clone()), None);
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
        let before = live.current_seq();

        let (mut rx, _) = live.follow(Some(before));
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

        match live.follow(Some(before)).1 {
            // A client that is one frame behind is handed the reset frame itself, which is
            // the mechanism working: it re-reads `/session` and gets the new conversation.
            // What it must never be handed is a frame of the old one.
            Catch::Missed(frames) => {
                assert_eq!(frames.len(), 1, "only the reset frame is left in the ring");
                let only = frames[0].render();
                assert!(only.contains("event: reset"), "{only:?}");
                assert!(
                    !only.contains("from the old conversation"),
                    "the old conversation survived the restart: {only:?}"
                );
            }
            Catch::Reset => panic!("a client one frame behind must be given the reset frame"),
        }

        // Further back than the ring can serve -- which is now every cursor from before the
        // restart -- and the answer is to reload rather than to guess.
        match live.follow(Some(before.saturating_sub(5))).1 {
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
        let mut viewer = Viewer::asked(0, None, None);
        let first = viewer.open(0).await.expect("bind");
        let second = viewer.open(0).await.expect("bind again");
        assert_eq!(first, second, "the second ask must report where it already is");
    }

    /// And a viewer that was only *asked* for is not broken -- it is unbound.
    #[test]
    fn a_viewer_that_has_not_been_opened_has_no_window() {
        let viewer = Viewer::asked(0, None, None);
        assert!(viewer.window.is_none());
        // The feed exists anyway, which is the point of the split: a turn that starts while
        // the socket is still being bound does not lose its frames.
        viewer.live().event(&Event::Text("while binding".to_string()));
        assert_eq!(viewer.live().current_seq(), 1);
    }

    #[test]
    fn a_client_that_has_just_loaded_is_told_nothing_it_missed() {
        let live = Live::new();
        live.event(&Event::Text("hello".to_string()));
        // `None` means "I have read /session and I am current", which is what the page
        // sends on its first connection.
        let (_, catch) = live.follow(None);
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
        let (_, catch) = live.follow(Some(1));
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
        match live.follow(Some(1)).1 {
            Catch::Reset => {}
            Catch::Missed(frames) => panic!("frame 1 is long gone, but {} were replayed", frames.len()),
        }
        // And the id just before the oldest kept one still works: the boundary is off by
        // one in the direction that loses nothing.
        let oldest = live.recent.lock().unwrap().front().map(|f| f.seq).expect("frames");
        match live.follow(Some(oldest)).1 {
            Catch::Missed(frames) => assert_eq!(frames.len(), RECENT_FRAMES - 1),
            Catch::Reset => panic!("the oldest kept frame is still replayable"),
        }
    }

    /// An id from a previous process -- or a made-up one -- is not trusted.
    #[test]
    fn an_id_ahead_of_anything_we_sent_is_told_to_reload() {
        let live = Live::new();
        live.event(&Event::Text("one".to_string()));
        assert!(matches!(live.follow(Some(9999)).1, Catch::Reset));
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
