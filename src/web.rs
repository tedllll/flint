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

use std::net::Ipv4Addr;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The most of a request head this server will read before giving up on it.
///
/// A browser's request head is under two kilobytes and this is four times that, so the only
/// thing the ceiling can be for is a client that never sends the blank line.
const MAX_HEAD: usize = 8 * 1024;

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
    session: Option<std::path::PathBuf>,
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
}

impl Response {
    fn text(status: u16, reason: &'static str, body: impl Into<String>) -> Response {
        Response {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
        }
    }

    /// One of the four refusals. The reason is short and honest: it is a person debugging
    /// their own URL that reads it, and the listener is on loopback, so there is nothing to
    /// learn from it that a local process could not read anyway.
    fn refused(why: &str) -> Response {
        Response::text(403, "Forbidden", format!("{why}\n"))
    }

    fn render(&self) -> Vec<u8> {
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
             \r\n",
            self.status,
            self.reason,
            self.content_type,
            self.body.len()
        );
        out.push_str(&self.body);
        out.into_bytes()
    }
}

/// One request, one response -- and every rule of §4 in one place.
///
/// Pure, so the boundary can be tested without a socket and without timing. The order is
/// deliberate: `Host` before anything else, because it is the check that decides whether the
/// browser's same-origin protection applies at all, and a request that fails it is not from
/// a page of ours however good its token looks.
pub fn respond(request: &Request, state: &State) -> Response {
    if !host_is_ours(request.header("host"), state.port) {
        return Response::refused("this listener answers only to 127.0.0.1 (check the Host header)");
    }
    if !origin_is_ours(request.header("origin"), state.port) {
        return Response::refused("this request came from another origin");
    }
    let allowed = match request.offered_token() {
        Some(offered) => token_matches(offered, &state.token),
        None => false,
    };
    if !allowed {
        return Response::refused("missing or wrong token (it is in the URL `--web` printed)");
    }

    match (request.method.as_str(), request.path()) {
        ("GET", "/") => Response {
            status: 200,
            reason: "OK",
            content_type: "text/html; charset=utf-8",
            body: VIEW_HTML.to_string(),
        },
        // The conversation so far, as the session file's own lines. Not a summary and not a
        // second format: the page is a reader of the same file the terminal is writing, and
        // the moment this route rendered something of its own the two could disagree.
        ("GET", "/session") => serve_session(state),
        ("GET", _) | ("HEAD", _) => Response::text(404, "Not Found", "no such route\n"),
        (_, "/") => Response::text(
            405,
            "Method Not Allowed",
            "the view is served, not written to\n",
        ),
        _ => Response::text(404, "Not Found", "no such route\n"),
    }
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
    let Some(path) = &state.session else {
        return Response::text(
            404,
            "Not Found",
            "this run is not writing a session, so there is nothing to show\n",
        );
    };
    match std::fs::read_to_string(path) {
        Ok(body) => Response {
            status: 200,
            reason: "OK",
            content_type: "application/x-ndjson; charset=utf-8",
            body,
        },
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
    pub async fn open(port: u16, session: Option<std::path::PathBuf>) -> Result<Window> {
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
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        // The end of the head is the whole of the request for every route so far.
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEAD {
            let response = Response::text(431, "Request Header Fields Too Large", "too long\n");
            let _ = stream.write_all(&response.render()).await;
            let _ = stream.shutdown().await;
            return Ok(());
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let response = match Request::parse(&head) {
        Some(request) => respond(&request, state),
        None => Response::text(400, "Bad Request", "not an HTTP/1.1 request\n"),
    };
    stream.write_all(&response.render()).await?;
    // `Connection: close` is not a suggestion: the response is over when the socket is, and
    // that is what removes keep-alive, pipelining and request framing from this file (§6).
    let _ = stream.shutdown().await;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            token: "0123456789abcdef0123456789abcdef".to_string(),
            port: 7777,
            session: None,
        }
    }

    /// Ask the pure router a question, as bytes a client would have sent.
    fn ask(head: &str, state: &State) -> Response {
        let request = Request::parse(head).unwrap_or_else(|| panic!("fixture must parse: {head:?}"));
        respond(&request, state)
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
        State { session: Some(path), ..state() }
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
        assert_eq!(ask(&ours("/events"), &state).status, 404, "not built yet");
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

    /// The URL is the contract: whatever it says must be what the listener accepts.
    #[tokio::test]
    async fn the_url_that_is_printed_is_the_one_that_opens_the_window() {
        let window = Window::open(0, None).await.expect("bind");
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
        let window = Window::open(0, None).await.expect("bind");
        let host = format!("127.0.0.1:{}", window.port());
        let response = get(window.port(), "/", Some(&host), None).await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!response.contains("<!doctype html>"), "the page leaked without a token");
    }

    /// Each run gets its own port and its own token, which is what makes either mean
    /// anything: a token that repeated between runs would be a password written down.
    #[tokio::test]
    async fn every_run_gets_its_own_port_and_its_own_token() {
        let first = Window::open(0, None).await.expect("bind");
        let second = Window::open(0, None).await.expect("bind");
        assert_ne!(first.port(), second.port(), "two listeners cannot share a port");
        assert_ne!(first.token, second.token, "two runs must not share a token");
        assert_eq!(first.token.len(), 32, "128 bits, as hex");

        // And the first window's token really does not open the second.
        let host = format!("127.0.0.1:{}", second.port());
        let response = get(second.port(), "/", Some(&host), Some(&first.token)).await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }
}
