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

/// One occurrence of the page, and the few lines that follow it.
///
/// Lines rather than a byte window: this file is UTF-8, and a slice taken at a byte offset can
/// land in the middle of a character -- which panics instead of failing the test it is in.
fn from(needle: &str, lines: usize) -> String {
    let rest = view()
        .split(needle)
        .nth(1)
        .unwrap_or_else(|| panic!("the page does not mention {needle:?}"));
    rest.lines().take(lines).collect::<Vec<_>>().join("\n")
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

/// The sidebar can start a conversation, not only switch between the ones that exist.
///
/// Reported as a basic gap after using the page: the list was there and "new" was not, so a page
/// could only ever show conversations somebody had already begun. The button is the small half;
/// the half worth testing is that it sends `/new` — the terminal's own command — rather than the
/// page growing an opinion about what starting a conversation means.
#[test]
fn the_sidebar_can_start_a_conversation() {
    let html = view();
    assert!(
        html.contains("id=\"new-conversation\""),
        "there must be a way to start one from the page"
    );
    assert!(
        html.contains("\"/new\""),
        "it must go through the command the terminal already has"
    );
    // And the composer says so, because nothing on the page did: the one command a person
    // reaches for is `/new`, and a composer that only looks like a message box hides the rest.
    assert!(
        html.contains("/help lists the commands"),
        "the composer must say that commands work here"
    );
}

/// The composer can stop the turn it is watching.
///
/// Everything else the page sends, a person types. `/new` has a button because the sidebar has
/// one, and the interrupt had nothing: the only way to stop a turn from the browser was to type a
/// slash command into a box that looks like it is for talking to the model -- which is the one
/// thing the hint line says *steers* instead. Three things have to hold, and each is a way this
/// can be wrong rather than merely absent:
///
/// * the word is `/stop`, sent through `sendText` like every other line, because a second verb for
///   stopping would be a second thing to keep in step with the REPL;
/// * it is a `type="button"`: an untyped `<button>` inside a form *submits* it, so a click would
///   send whatever is in the textarea instead of stopping anything;
/// * it is offered only while a turn is in flight, read from the turn's own boundaries
///   (`doc.running`) and not from the status line, which also carries feed trouble. A stop button
///   that appears because the connection dropped is a button that stops nothing.
#[test]
fn the_composer_can_stop_the_turn_it_is_watching() {
    let html = view();
    let button = html
        .lines()
        .find(|line| line.contains("<button id=\"stop\""))
        .expect("the composer has no stop button, so a turn can only be typed to a stop");
    assert!(
        button.contains("type=\"button\""),
        "a button in a form submits it unless it says otherwise: {button:?}"
    );
    assert!(
        button.contains("hidden"),
        "the button must start out hidden, or it is offered before there is anything to stop: {button:?}"
    );

    // The click handler, not the first mention of the button: the page also re-enables it when a
    // turn starts, and a window that began there would not reach the handler at all.
    let handler = from("getElementById(\"stop\").addEventListener", 12);
    assert!(
        handler.contains("sendText(\"/stop\")"),
        "the stop button must send /stop: {handler}"
    );
    assert_eq!(
        handler.matches("sendText(").count(),
        1,
        "the stop button sends exactly one thing, and it is /stop: {handler}"
    );

    assert!(
        from("stop.hidden =", 1).contains("doc.running"),
        "the button is shown for a turn in flight and for nothing else"
    );
    assert!(
        from("case \"turn.started\"", 5).contains("doc.running = true"),
        "a turn starting is what puts the button there"
    );
    assert!(
        from("case \"turn.completed\"", 8).contains("doc.running = false"),
        "and the end of the turn is what takes it away"
    );
}

/// The pickers are the run's own commands, with the choices laid out.
///
/// §8's first control, and the reason the read channel came first: a picker cannot be drawn
/// without knowing the options, and the page may not read `config.toml` for them -- a second
/// reader of the same state is a second thing that can disagree with the process, which is not
/// hypothetical here (`/config` prints the file's `readonly` beside the value in force, and they
/// differ for a whole class of runs). So the options arrive in the `state` frame, and what is
/// checked here is the half that has an answer in the source rather than in a browser: where the
/// controls are offered, and what each one sends.
///
/// Three things have to hold, and each is a way this can be wrong rather than merely absent:
///
/// * the markup starts hidden, because a page opened from a dropped file has no process behind it
///   and a picker with no options is a promise the page cannot keep;
/// * each picker sends exactly one line -- `/provider <name>`, `/model <name>` -- through
///   `sendText`, so the terminal and the page cannot come to disagree about what those mean;
/// * the `state` frame is applied where it arrives, and not left to fall through to `applyLine`,
///   where a named frame's data would be read as a line of the run's vocabulary and skipped in
///   silence. That failure would look exactly like the feature not being built.
#[test]
fn the_pickers_offer_the_runs_own_commands() {
    let markup = from("<div class=\"controls\" id=\"controls\" hidden>", 5);
    assert!(
        markup.contains("id=\"pick-provider\"") && markup.contains("id=\"pick-model\""),
        "the header has no pickers to draw into: {markup}"
    );

    for (id, command) in [("pick-provider", "/provider "), ("pick-model", "/model ")] {
        // Three lines: the handler is one call and one statement, and a wider window would reach
        // into the *other* picker's handler and count its `sendText` as this one's.
        let handler = from(&format!("getElementById(\"{id}\").addEventListener"), 3);
        let line = format!("sendText(\"{command}\" + e.target.value)");
        assert!(
            handler.contains(&line),
            "the {id} picker must send `{command}<value>`: {handler}"
        );
        assert_eq!(
            handler.matches("sendText(").count(),
            1,
            "the {id} picker sends exactly one thing: {handler}"
        );
    }

    assert!(
        from("if (frame.event === \"state\")", 5).contains("applyState(doc, frame.data)"),
        "the state frame has to be applied where it arrives"
    );
    // And the page's only source for those options is the frame: no route of its own, and no
    // reading of the file the process owns.
    assert!(
        !view().contains("fetch(\"/config\""),
        "the page must be told the state, not read the config itself"
    );
}

/// A conversation archived or deleted in the terminal has to leave the sidebar.
///
/// Reported from a real session: history tidied in the terminal and the page still offering it.
/// The list being wrong is the small half. The numbers in it are what `/resume` takes, and they
/// are *positions in a list* — delete one conversation and every number below it shifts up — so a
/// stale sidebar resumes the wrong conversation and the person carries on talking in it.
#[test]
fn the_page_re_reads_the_list_when_the_process_says_it_changed() {
    let html = view();
    let branch = html
        .split("frame.event === \"sessions\"")
        .nth(1)
        .expect("the page must handle the list-changed event");
    let branch = &branch[..branch.len().min(300)];
    assert!(
        branch.contains("readSessions()"),
        "the sidebar has to be re-read: {branch:?}"
    );
    // And *only* the sidebar. The transcript has not changed, so rebuilding it would throw away
    // the reader's place for nothing -- which is the difference between this event and `reset`.
    assert!(
        !branch.contains("readSession()") || branch.contains("readSessions()"),
        "the list changing must not rebuild the transcript: {branch:?}"
    );
    assert!(
        !branch.contains("await readSession();"),
        "the transcript is not stale when only the list changed: {branch:?}"
    );
}
