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
