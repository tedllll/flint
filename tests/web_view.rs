//! What the browser view is not allowed to do, checked over the bytes the binary serves.
//!
//! The page cannot be unit-tested by `cargo test`: it needs a DOM, and a DOM in the test
//! process would be a dependency and a fiction. So the tests go *around* it, and this is the
//! half that can only be a text scan.
//!
//! Two mistakes are worth this much machinery, and both are the kind that are invisible in a
//! diff: **an injection hole** and **an external dependency**. The first matters more here
//! than in most programs, because the text this page renders is attacker-influenced -- a
//! file's contents, a program's stderr, a fetched page -- and the program rendering it can
//! run commands. The second matters because the whole argument for a local listener is that
//! nothing leaves the machine.
//!
//! The scanner is blunt on purpose. A clever test that understood JavaScript would be a
//! second parser to be wrong about, and would pass a page that fetched a font.
//!
//! The *behaviour* of the renderer is checked by `scripts/web-view-test.js`, which runs the
//! same embedded script under Node with a stub DOM and asserts what it does with a real
//! session file.

/// The page as the listener will serve it.
fn view() -> &'static str {
    flint::web::VIEW_HTML
}

/// Every occurrence of `needle`, with the line it is on, for an error that names the place.
fn sites(haystack: &str, needle: &str) -> Vec<String> {
    haystack
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(n, line)| format!("  line {}: {}", n + 1, line.trim()))
        .collect()
}

/// Assert a forbidden construct appears nowhere, and say where it does when it does.
fn forbidden(needle: &str, why: &str) {
    let found = sites(view(), needle);
    assert!(
        found.is_empty(),
        "{why}\nfound {needle:?} at:\n{}",
        found.join("\n")
    );
}

/// The test is worthless if the page is empty, so say what it must contain before the
/// negative assertions, which would all pass over an empty string.
#[test]
fn the_view_is_actually_there_and_renders_through_text_content() {
    let html = view();
    assert!(
        html.len() > 1000,
        "the embedded view is {} bytes, which is too small to be the viewer: {html:?}",
        html.len()
    );
    assert!(html.contains("<!doctype html>"), "not an HTML document");
    assert!(
        html.contains("textContent"),
        "the page must put text in through textContent, and does not mention it"
    );
    // A page with no script and no stylesheet would pass every negative assertion below.
    assert!(html.contains("<script>"), "the view has no script");
    assert!(html.contains("<style>") || html.contains("<link"), "no styles");
}

/// Model output and tool output are set as text, never as markup.
#[test]
fn the_view_never_assigns_markup() {
    forbidden("innerHTML", "markup assignment is a script-injection hole here");
    forbidden("outerHTML", "markup assignment is a script-injection hole here");
    forbidden("insertAdjacentHTML", "markup assignment is a script-injection hole here");
    forbidden("document.write", "document.write reparses markup");
    forbidden("eval(", "eval over model output is the injection hole with extra steps");
    // A Markdown renderer is a parser, and a parser that emits HTML is the same hole. The
    // document says plain text in `<pre>`; a `marked(` or `md(` call would be a dependency
    // and a decision reversed without saying so.
    forbidden("marked(", "no Markdown renderer: it is a parser that emits HTML");
}

/// Nothing is loaded from anywhere, and there is nothing to load it with.
#[test]
fn the_view_requests_nothing_external() {
    for needle in ["http://", "https://", "//cdn", "<script src", "<link rel=\"stylesheet\" href=\"http"] {
        forbidden(needle, "the view must not reach off the machine");
    }
    // No absolute URL anywhere means a relative `fetch` is fine later, which is exactly
    // what `/session` and `/events` will need. This is why the check is for absolute URLs
    // rather than for `fetch`.
    forbidden("XMLHttpRequest", "the view has no reason to use XHR");
}

/// The two rules the session format asks of *any* reader, stated where a page author will
/// see them: the page is a reader of the same files.
#[test]
fn the_view_skips_unknown_events_and_shows_damage() {
    let html = view();
    assert!(
        html.contains("skipped in silence"),
        "the page must know that an unknown event type is skipped, not shown or fatal"
    );
    assert!(
        html.contains("damage"),
        "a known type that will not parse is damage, and the page must say so"
    );
}

/// The page knows both vocabularies, because it is fed by both: a session file now, and the
/// event stream over `/events` next.
#[test]
fn the_view_understands_both_vocabularies() {
    let html = view();
    for kind in [
        // a session file
        "\"meta\"", "\"chat\"", "\"title\"",
        // the NDJSON stream
        "\"message.delta\"", "\"tool.completed\"", "\"turn.started\"", "\"status\"",
    ] {
        assert!(
            html.contains(kind),
            "the view does not handle {kind}, which it will be fed"
        );
    }
}

/// The composer posts the token in a header, never in a query string.
///
/// §4.2 again, and it matters more here than anywhere: `/message` is the route that can make
/// flint do something. A token in a query string ends up in history, in a log and in any link
/// that gets shared, and `?token=` is accepted on `/` *alone* precisely so that pasting the URL
/// works without every other request carrying a credential in its address.
#[test]
fn the_composer_keeps_the_token_out_of_the_url() {
    let html = view();
    assert!(
        html.contains("\"/message\""),
        "the page must post messages to the route that exists"
    );
    assert!(
        html.contains("\"/sessions\""),
        "the sidebar must read the list from the route that exists"
    );
    // Both go through the one helper that builds the header, and neither appends a query.
    for route in ["/message?", "/sessions?", "/events?token", "/session?token"] {
        assert!(
            !html.contains(route),
            "{route} would put the token in an address bar, a log and a Referer"
        );
    }
}

/// A message that was refused must not be shown as if it had been sent, and a message typed
/// while an earlier one is in flight must not be erased by it.
///
/// One line, two ways to get it wrong, and both were found by driving a real browser rather
/// than by reading this file. The page cannot append what it typed and call that the
/// transcript: the transcript is what flint was *told*, and the only thing that can say whether
/// it was told is the reply. Nor can it clear the box on a timer of its own -- two submits can
/// overlap, because typing again during the round trip is ordinary, and the earlier clear then
/// wipes the later text. Measured: send, type again within 150 ms, send; the second message was
/// written and then erased before it could be read.
#[test]
fn the_composer_is_honest_about_what_it_sent() {
    let html = view();
    assert!(
        html.contains("not sent:"),
        "a refused message must be reported, not silently dropped"
    );
    let handler = html
        .split("addEventListener(\"submit\"")
        .nth(1)
        .expect("the form must have a submit handler");
    let cleared = handler
        .find("message.value = \"\"")
        .expect("the input must be cleared somewhere");
    let checked = handler
        .find("await sendText(text) && message.value === text")
        .expect("the clear must be conditional on acceptance *and* on the box still holding it");
    assert!(
        checked < cleared,
        "the input is cleared without checking what came back"
    );
}
