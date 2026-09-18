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
///
/// A link a person presses is not the page reaching anywhere: that navigation belongs to the
/// browser, in a new tab, and it is asserted in `an_address_leaves_the_page_and_a_path_does_not`.
/// What is forbidden here is a **subresource** -- a script, a stylesheet, a fetch -- because that is
/// the page itself making a request, and a page served off this machine has no business doing that
/// at all.
#[test]
fn the_view_requests_nothing_external() {
    for needle in ["http://", "https://", "//cdn", "<script src", "<link rel=\"stylesheet\" href=\"http"] {
        forbidden(needle, "the view must not reach off the machine");
    }
    // No absolute URL anywhere means a relative `fetch` is fine later, which is exactly
    // what `/session` and `/events` will need. This is why the check is for absolute URLs
    // rather than for `fetch`.
    forbidden("XMLHttpRequest", "the view has no reason to use XHR");
    // ...and the two spellings of the same mistake now that the page does name a scheme: a fetch or
    // a tag pointed at somewhere else. The link's own `href` is a property set from the address in
    // the text, so it never appears as one of these.
    for needle in ["fetch(\"http", "fetch('http", "fetch(`http", "src=\"http", "href=\"//"] {
        forbidden(needle, "the page must not request anything off this machine");
    }
}

/// A path in a tool block is a thing to press, and it opens **in this page**.
///
/// The rule this pins is not cosmetic. A link would navigate away from a conversation that is
/// still running -- the token, the stream and the reader's place all live in this document -- and
/// a `window.open` would be a second copy of the page with none of them. What the page may do
/// instead is ask the run for the file, over the one route that reads a path (`GET /file`).
///
/// An address on the *web* is the other kind, and it is a link in a new tab: see
/// `an_address_leaves_the_page_and_a_path_does_not`, which is where that half is asserted. The two
/// are split on purpose -- a path has no address a browser could open, and a URL has no file for
/// `GET /file` to read.
#[test]
fn a_path_opens_in_this_page_or_not_at_all() {
    let html = view();
    assert!(
        html.contains("function fileRoute(path)"),
        "the route a path is read through must be one function, so the encoding is one decision"
    );
    assert!(
        html.contains("\"/file?path=\" + encodeURIComponent(path)"),
        "the path travels percent-encoded: a raw backslash or space is a different request"
    );
    assert!(
        html.contains("fetch(fileRoute(preview.path)"),
        "the panel reads the file through the route and not by any other means"
    );
    assert!(
        html.contains("el(\"button\", \"path\", part.path)"),
        "a path is a button, which cannot navigate the page"
    );
    forbidden("window.open(", "a file opens in this page, where the run that served it is");
    // The one place a path could reach the wire unencoded, or as markup, is the panel's header:
    // it is built from `textContent` like everything else.
    let head = from("function paintPreviewHead()", 14);
    assert!(
        !head.contains("innerHTML") && head.contains("textContent"),
        "the panel's header is text like every other piece of the page:\n{head}"
    );
}

/// An address on the web is a link out of this page, and it is the only thing that may be.
///
/// This is a change of position, so it is written down as one. The page used to contain **no
/// absolute URL anywhere** and the rule was that a path is a button "because a link would navigate
/// away"; a web address reverses half of that: it has somewhere real to go, and the only way to
/// open it without destroying this document -- the token, the stream, the reader's place -- is a
/// new tab with the opener severed. So a path stays a button and an address becomes a link, and the
/// tests keep the two apart rather than weakening the path's rule.
///
/// The scheme is the part that has to be an allowlist. This document holds the run's token, and a
/// model writes the text it renders: an `href` of `javascript:...` is script running in it, needing
/// no bug and no parser, only a click. `http` and `https` are the two schemes that mean "a page",
/// and the check below is that they are tested in one place -- a function -- rather than assumed
/// where the anchor is built.
#[test]
fn an_address_leaves_the_page_and_a_path_does_not() {
    let html = view();
    assert!(
        html.contains("function asUrl(token)"),
        "the one place a scheme is accepted must be one function, so there is one allowlist"
    );
    assert!(
        html.contains("/^https?:\\/\\/[^\\s/]+/i.test(text)"),
        "the test is an allowlist of the two web schemes, and it wants a host after them"
    );
    assert!(
        html.contains("link.target = \"_blank\";"),
        "a page opens in a new tab: this document is the conversation it was read in"
    );
    assert!(
        html.contains("link.rel = \"noopener noreferrer\";"),
        "and the tab it opens cannot reach back through `window.opener`"
    );
    // Prose is read with the narrower rule -- absolute paths only -- because a sentence is where
    // `and/or` and `e.g.` and `src/bin` all live. This assertion is that the *view* uses it: a rule
    // nobody calls is not a rule.
    assert!(
        html.contains("linkNodes(block.text, true)"),
        "a turn's own words are read with the prose rule"
    );
    forbidden("href = part.text", "an address is a property, never markup");
}

/// The jobs panel reads the run's list, and a row is a press that stays in this page.
///
/// Two policies, in one place because they are the same policy seen twice. The list is a *route*
/// (`GET /jobs`) that a frame says is stale -- carrying the jobs on the stream would be a second
/// answer that can disagree with the route -- and a row is a door onto a job's output, through the
/// same `GET /file` a path in the transcript uses, so nothing about a job can navigate the page away
/// from the conversation it belongs to.
#[test]
fn the_jobs_panel_reads_the_route_and_a_row_stays_in_this_page() {
    let html = view();
    assert!(
        html.contains("fetch(\"/jobs\", { headers: authHeader() })"),
        "the list comes from the route, with the token in the header like every other request"
    );
    assert!(
        html.contains("frame.event === \"jobs\""),
        "and the frame only says to re-read it"
    );
    assert!(
        html.contains("openPreview(job.path, 0)"),
        "a row opens the job's output in the preview column, not in a second window"
    );
    // The panel is drawn from the route's fields, and each row is text: a command line is a command
    // line, and the exit code is a sentence the route wrote rather than one the page made up. The
    // text arrives through `el`, which is the one place on this page that sets `textContent`.
    let row = from("function jobRow(job)", 40);
    assert!(
        !row.contains("innerHTML") && row.contains("el(\"span\", \"dot\")"),
        "a row is built from elements and text like the rest of the page:\n{row}"
    );
    assert!(
        row.contains("job.detail"),
        "the exit code the route sent is what the row shows:\n{row}"
    );
    // And the ticking, which must not be a request: the page counts from the absolute times the
    // route sent, so a page open for an hour is still right about a job it heard about at the start.
    let tick = from("function tickJobs()", 16);
    assert!(
        !tick.contains("fetch("),
        "the clock is arithmetic, not a poll:\n{tick}"
    );
    assert!(
        tick.contains("Date.now()"),
        "and it counts from the reader's own clock:\n{tick}"
    );
}

/// The header names the conversation, and says what the run is doing with it.
///
/// Two facts on one line, and neither is invented: the name is the session file's own newest `title`
/// event with the label from `GET /sessions` behind it (the newest name, else the first thing that
/// was said, else "(empty)" -- chosen in one place in `session::list`), and the work is the same
/// `Job` record the jobs panel, the terminal's `/jobs` and `job_op` all answer from. What this
/// guards against is a header that says `flint` while three subagents are working: the product's name
/// where the conversation's belongs, and no sign that anything is running at all. DSH's session
/// header carries the same two facts in the same place -- the session's display title, and a job
/// badge that is a count of what is live, rendered not at all when nothing is.
#[test]
fn the_header_names_the_conversation_and_what_the_run_is_doing() {
    let line = from("<div class=\"head-line\">", 14);
    assert!(
        line.contains("id=\"title\"") && line.contains("id=\"jobs\""),
        "the name and the work are not on one line together:\n{line}"
    );

    // The name: the file's own title first, the list's label behind it, and the product's name only
    // when a page has neither. The order matters and is asserted, because "flint" is always a
    // *readable* answer and a page that fell back to it too early would look right and say nothing.
    let words = from("function titleWords(doc, label)", 4);
    assert!(
        words.contains("doc.title") && words.contains("label"),
        "the header's name is not read from the conversation:\n{words}"
    );
    let list = from("function paintSessions()", 12);
    assert!(
        list.contains("session.current") && list.contains("currentLabel") && list.contains("paintTitle(doc)"),
        "the name behind the title is not the conversation the run is holding:\n{list}"
    );

    // The work: counts of kinds rather than one total, and a failure kept out of `done` -- a job that
    // failed and one that finished are the same number in a total, and the row behind the chip is the
    // only place the exit code is legible.
    let chips = from("function paintJobs(list)", 34);
    for (needed, why) in [
        ("jobIsLive", "which jobs have not ended"),
        ("job.kind === \"child\"", "how many of them are subagents"),
        ("\"failed\"", "the ones that ended badly"),
        ("\"killed\"", "including the ones this run ended itself"),
        ("chip(", "the counts, drawn as the chips they are"),
    ] {
        assert!(
            chips.contains(needed),
            "the header's work chips do not read `{needed}` ({why}):\n{chips}"
        );
    }
}


///
/// A destructive row takes two presses, and the second one offers the *choices* -- which the page
/// takes from a list it already holds rather than from the frame, because the frame is a menu of
/// commands and a candidate is not a command. For `/jobs stop <pid>` that list is the jobs panel's
/// own rows: the pids are already on screen, which is what makes the stop a thing a person can aim.
/// Two rules come with it. Only the jobs a press can still *do* something about are offered -- a job
/// that has ended cannot be stopped, and one that is already stopping has already been asked -- and
/// the choice's label says *which* job, because two pids in a menu that are bare numbers are a menu
/// nobody can use. That is `jobIsStoppable` rather than `jobIsLive`, which is the wider question the
/// header's chips ask: a stopping job is still spending time, so it is counted and its clock keeps
/// running, but it is not offered a second stop.
#[test]
fn the_page_stops_a_job_from_the_rows_it_is_already_showing() {
    let body = from("const choices =", 22);
    assert!(
        body.contains("command.from === \"jobs\""),
        "the third list of candidates is not resolved:\n{body}"
    );
    assert!(
        body.contains("listedJobs"),
        "the candidates are the jobs panel's rows, not a second list:\n{body}"
    );
    assert!(
        body.contains("jobIsStoppable(job)"),
        "a job that has already ended, or is already stopping, is not something to stop:\n{body}"
    );
    assert!(
        body.contains("String(job.pid)"),
        "the value sent is the pid, and it is the thing the command takes:\n{body}"
    );
    assert!(
        body.contains("job.kind + \" \" + job.label"),
        "and the choice says which job it is:\n{body}"
    );
}

/// A file the page cannot show says why, in the route's own words.
///
/// The alternative -- an empty panel, or a spinner that stops -- is the failure this project keeps
/// designing against: three different reasons look exactly the same. `serve_file` writes a
/// sentence for each one (nothing there, a directory, not text, too big), and the page shows it.
#[test]
fn the_preview_shows_the_routes_own_refusal_rather_than_an_empty_panel() {
    let body = from("async function readPreview()", 30);
    assert!(
        body.contains("text.textContent = body.trim()"),
        "the refusal is shown as it was written:\n{body}"
    );
    assert!(
        body.contains("HTTP \" + response.status"),
        "and the status beside it, for the reader who wants the code:\n{body}"
    );
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
    // The composer's own handler, named rather than taken as "the first submit handler in the
    // file": a form row in the panel has a submit handler too, and this test is about the box that
    // sends a message.
    let handler = html
        .split("getElementById(\"composer\").addEventListener(\"submit\"")
        .nth(1)
        .expect("the composer must have a submit handler");
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

/// A toggle is a switch that shows its value, and the value it shows comes from the run.
///
/// §8 again, and the failure mode is specific: a control built from a list the page carries is a
/// control that can offer a word the command refuses, or show a value the run is not on. Three
/// of them matter here -- `/verbose on|off|full`, `/detail on|off` and `/readonly on|off` -- and
/// the page knows none of those words: it is handed a name, the values that name takes and the
/// value in force, and sends `/<name> <value>`. That is one command line, spelling and all, which
/// is the rule the whole of §8 rests on.
#[test]
fn the_toggles_are_switches_that_show_their_value() {
    // The markup has to have somewhere to put them, inside the controls a state frame reveals.
    let controls = from("<div class=\"controls\" id=\"controls\" hidden>", 8);
    assert!(
        controls.contains("id=\"toggles\""),
        "the header has nowhere to draw the switches: {controls}"
    );

    // Built from the frame, not from a table in the page.
    let built = from("function showToggles(doc)", 12);
    for (needed, why) in [
        ("toggle.name", "the command a switch sends"),
        ("toggle.values", "the values that command takes"),
        ("toggle.value", "the value in force"),
    ] {
        assert!(
            built.contains(needed),
            "the switches are not drawn from the frame's `{needed}` ({why}): {built}"
        );
    }

    // And what a change sends is the terminal's own line, composed from those two.
    let handler = from("select.addEventListener(\"change\"", 3);
    assert!(
        handler.contains("sendText(\"/\" + toggle.name + \" \" + e.target.value)"),
        "a switch must send the command it is named after: {handler}"
    );
    assert_eq!(
        handler.matches("sendText(").count(),
        1,
        "a switch sends exactly one thing: {handler}"
    );
}

/// A command's answer is a block in the transcript, with the line that asked for it.
///
/// §8's other half, over the page's own bytes. The composer sends whatever is typed to the REPL,
/// which is what makes every command reachable from here -- and until this, a command's answer
/// went to the terminal and nowhere the page could read, so `/config` typed into the composer
/// printed nothing at all. The block is not a notice: the answer is often a listing, and the input
/// is half of what a reader needs, since an answer with no question above it is a mystery and a
/// question with nothing under it is a program that ignored you.
#[test]
fn a_command_answer_is_a_block_with_the_line_that_asked_for_it() {
    // In the vocabulary the page accepts, or the handler below is dead code: an event type the
    // page does not list is skipped in silence, which is the format's own rule.
    let known = from("const KNOWN = new Set([", 12);
    assert!(
        known.contains("\"command\""),
        "the page does not know the event a command's answer arrives as: {known}"
    );

    // The block, built from the frame's own two fields. The window is wide enough to reach past the
    // branch above it, which is the panel's copy of the same frame.
    let handler = from("case \"command\":", 20);
    assert!(
        handler.contains("kind: \"command\"")
            && handler.contains("text(ev.input)")
            && handler.contains("text(ev.text)"),
        "the answer is not built from the input and the text the frame carries: {handler}"
    );

    // And drawn with the input as its label: a block whose body appears under the previous
    // message's label reads as part of that message.
    let painted = from("if (block.kind === \"command\")", 1);
    assert!(
        painted.contains("el(\"div\", \"who\", block.input)"),
        "a command's answer is drawn without saying which command it answers: {painted}"
    );
}

/// The command panel is drawn from the frame, and the page carries none of the list itself.
///
/// §8's read channel, second half over the page's own bytes. Two things are checked, and the second
/// is the one that matters: the panel is built from `state.commands`, and the page contains no
/// command name of its own. A page that knew `/provider key <key>` would be a second copy of the
/// terminal's grammar — it would go on offering a command that was renamed, in a panel whose whole
/// job is to say what there is.
#[test]
fn the_command_panel_is_drawn_from_the_frame() {
    // Somewhere to put it, in the header a state frame reveals. The window runs past the controls
    // div: the panel is a sibling of it rather than a child, because `.controls` is a flex row and
    // a panel belongs on its own line.
    let controls = from("<div class=\"controls\" id=\"controls\" hidden>", 20);
    assert!(
        controls.contains("id=\"commands\"") && controls.contains("id=\"command-list\""),
        "the header has nowhere to draw the command panel: {controls}"
    );

    // Built from the frame: every field of a row comes from the frame's own entry.
    let built = from("function showCommands(doc)", 40);
    for (needed, why) in [
        ("state.commands", "the list of commands"),
        ("command.label", "what the row says to type"),
        ("command.help", "what the row says it does"),
        ("command.class", "which group the row belongs to"),
    ] {
        assert!(
            built.contains(needed),
            "the panel is not drawn from the frame's `{needed}` ({why}): {built}"
        );
    }

    // And the page holds no copy of the command list. These are the strings that would be in it if
    // it did; the panel's *group* names are the page's own, and are checked above instead.
    let html = view();
    for leaked in ["/provider key", "/delete <n|id>", "/reload", "inspect the config"] {
        assert!(
            !html.contains(leaked),
            "the page carries `{leaked}` itself, so it can offer a command the terminal does not \
             have"
        );
    }
}

/// The action buttons are drawn from the frame too, and send the row's own line.
///
/// §8's first control, and the reason it is first: an action takes no argument, so there is nothing
/// to ask for and nothing to confirm — the whole control is "send this line", which is what the
/// composer already does. What is checked here is the part that can go wrong: the line is the
/// frame's `send`, not a name this page reassembles, and a press that is refused puts the header
/// back to what is in force rather than leaving a control that quietly did nothing.
#[test]
fn the_action_buttons_send_the_frames_own_line() {
    let controls = from("<div class=\"controls\" id=\"controls\" hidden>", 20);
    assert!(
        controls.contains("id=\"actions\""),
        "the header has nowhere to draw the actions: {controls}"
    );

    let drawn = from("function showActions(doc)", 30);
    for (needed, why) in [
        ("state.commands", "the list of commands"),
        ("command.class !== \"button\"", "the class that makes a command an action"),
        ("sendText(command.send)", "the line, taken from the frame rather than rebuilt here"),
        ("showState(doc)", "putting the control back when the send is refused"),
    ] {
        assert!(
            drawn.contains(needed),
            "the buttons are not drawn from `{needed}` ({why}): {drawn}"
        );
    }
    // The label is the command's own words (`/reload`), which is what `/help` prints, and the help
    // line is the tooltip: the row already carries both, and a page that shortened one would be
    // inventing a second name for a command.
    assert!(
        drawn.contains("command.label") && drawn.contains("command.help"),
        "a button is not labelled from the frame's own row: {drawn}"
    );
}

/// A report row reads in the panel rather than sending into the transcript.
///
/// §8's second class, and the one with a route of its own: `/report` runs a command with the
/// terminal *quiet*, so a listing the page asked for does not also print where somebody typed
/// `/help`. This test is the page's half of that -- the row is pressable, the line is the frame's
/// `send`, the answer goes to the panel, and a report is never a transcript block. Which commands
/// may be read at all is not this page's business: the frame says `class: "panel"` for the ones that
/// may, and the process refuses the rest.
#[test]
fn the_panel_reads_a_report_rather_than_sending_it() {
    // The row itself: only the class the frame marks, and the line comes from the frame.
    let built = from("function showCommands(doc)", 180);
    for (needed, why) in [
        ("className === \"panel\"", "the class that makes a row readable"),
        ("command.send", "the line to ask for, taken from the frame rather than rebuilt here"),
        ("askReport(doc, item.line)", "asking the process for the line the row offers"),
        ("command.values", "the values the frame says this row may be given"),
        ("send + \" \" + value", "the line for one of them, composed from the frame's own strings"),
    ] {
        assert!(
            built.contains(needed),
            "the panel does not read its rows through `{needed}` ({why}): {built}"
        );
    }

    // The request: the route is what keeps the answer off the terminal, so a page that posted to
    // `/message` here would be a panel that printed into the terminal -- the exact thing §8's class
    // exists to prevent.
    let asked = from("async function askReport(doc, input)", 30);
    for (needed, why) in [
        ("fetch(\"/report\"", "the route that answers without printing"),
        ("messageBody(input)", "the line, in the same body shape the composer sends"),
        ("doc.reading = input", "the panel showing what is being read"),
        ("showCommands(doc)", "redrawing the panel rather than the transcript"),
    ] {
        assert!(
            asked.contains(needed),
            "asking for a report does not use `{needed}` ({why}): {asked}"
        );
    }
    assert!(
        !asked.contains("/message"),
        "a report is sent to the composer's route, so the listing would print in the terminal: \
         {asked}"
    );

    // And the answer: a `command` frame marked as a panel's fills the panel, and the unmarked shape
    // -- what a typed command produces -- still becomes a transcript block.
    let handled = from("case \"command\":", 20);
    for (needed, why) in [
        ("ev.panel === true", "the mark that says this answer belongs in the panel"),
        ("doc.readingText", "filling the reading rather than appending to it"),
        ("showCommands(doc)", "redrawing the panel"),
    ] {
        assert!(
            handled.contains(needed),
            "a panel's answer is not put in the panel through `{needed}` ({why}): {handled}"
        );
    }
    assert!(
        handled.contains("kind: \"command\"") && handled.contains("push(doc,"),
        "a command's answer stopped being a transcript block, so a typed `/config` would show \
         nothing: {handled}"
    );
}

/// A row the frame marks as taking a field gets one, and what it sends is still the frame's.
///
/// §8's form class, and the only class whose argument a person types freely. The page draws an input
/// because the *frame* says so (`field` is the input's `type`), composes the line as `send` plus what
/// was typed -- the same composition a toggle and a value row use -- and posts it where a typed line
/// goes, because a form changes something and its answer belongs in the transcript.
///
/// The masked case is the one with a promise attached: a password field is emptied the moment it is
/// sent, so the value lives in the input, on the wire, and in the config file it was for, and nowhere
/// else on the page. What the *process* does with it -- the frame's own echo, which would otherwise
/// hand the key back to every page watching -- is measured in `tests/cli_output.rs`.
#[test]
fn a_form_row_gets_a_field_and_sends_what_was_typed_into_it() {
    let drawn = from("function showCommands(doc)", 130);
    for (needed, why) in [
        ("command.fields", "the frame saying this row takes answers, and of which kinds"),
        ("input.type = spec && spec.field === \"password\" ? \"password\" : \"text\"", "drawing each as the frame said rather than by guessing from the name"),
        ("formLine(send, fields, inputs.map((input) => input.value))", "the line being built by the one function that knows what the answers become"),
        ("if (!line) return;", "nothing being sent while an answer the frame did not mark optional is missing"),
        ("fields[at].field === \"password\"", "keeping no copy of a credential"),
    ] {
        assert!(
            drawn.contains(needed),
            "a form row is not drawn from `{needed}` ({why}): {drawn}"
        );
    }
}

/// The answers of a form become one line, and a missing one is left out or refuses.
///
/// The half a drawing cannot show. `send` with no answers is the command's *interactive* form --
/// `/provider add` bare is a wizard that asks a person for five things one at a time -- so a page that
/// sent a partly-filled form would hand the run to a wizard whose only reader is a terminal nobody is
/// sitting at. Which answers are required is the frame's word (`optional`), not the page's guess from
/// the label, for the same reason the field's kind is: a page that decided would be re-deriving this
/// program's grammar.
///
/// An optional answer that is empty is now *skipped* rather than ending the line, and the difference is
/// `/say [--to <pid>] <text>` (ROADMAP section 11 item 9(ii)): a flagged answer is a phrase of its own,
/// so an optional one can stand before a required one. For the positional rows this table has always
/// had, an optional answer is last by construction and skipping reaches the place stopping did.
#[test]
fn the_answers_of_a_form_become_one_line_and_a_missing_one_stops_it() {
    let answers = from("function formLine(send, fields, values)", 26);
    assert!(
        answers.contains("if (spec.optional) continue;"),
        "an empty optional answer is not left out of the line: {answers}"
    );
    assert!(
        answers.contains("if (!answer) {\n      //") && answers.contains("if (spec.optional) continue;\n      return null;"),
        "a required answer that is empty does not stop the line, so the bare command is sent: \
         {answers}"
    );
    // The flag is written before the answer it belongs to, which is what makes an address a phrase
    // rather than a word in the sentence.
    assert!(
        answers.contains("if (spec.flag) answers.push(spec.flag);"),
        "an answer that follows a flag is written without it, so the line reaches the terminal as \
         prose: {answers}"
    );
    assert!(
        answers.contains("[send].concat(answers).join(\" \")"),
        "the line is not the row's own `send` followed by the answers in the frame's order: {answers}"
    );
    // And the page uses it: this is the wiring, and the refusal above is decoration without it. Both
    // halves are asserted here rather than in the drawing test because this is the test whose subject
    // is the refusal -- a mutation that deletes the `if (!line) return;` line leaves the drawing
    // intact and the promise broken.
    let page = view();
    assert_eq!(
        page.matches("formLine(send, fields, inputs.map((input) => input.value))").count(),
        1,
        "the form's submit handler does not go through `formLine`, so the answers are joined twice \
         and the refusal above is about a line nobody sends"
    );
    assert!(
        page.contains("if (!line) return;"),
        "the handler sends what `formLine` refused, so an incomplete form is submitted as the bare \
         command -- which is the wizard, waiting for a person who is not at the terminal"
    );
}

/// The sidebar's rename row is the frame's own `/name` row, composed the way every other form is.
///
/// The drawing is checked under Node (`scripts/web-view-test.js` runs the page against a stub DOM);
/// what belongs here is the composition, because a name is the one answer a person types freely. The
/// page must not build `/name " + value` itself: `formLine` is what trims and what refuses an empty
/// answer, and a second composition is how the panel's forms and the sidebar's would come to disagree
/// about a name with two spaces in it.
///
/// The refusal is the half that matters. `/name` with no text is not an error -- it *reports* the
/// name -- so a submit that fell through an emptied field would turn a cleared field into a question
/// about the conversation's name, and the answer would land in the transcript as a command nobody
/// asked for.
#[test]
fn the_sidebar_renames_a_conversation_through_the_same_form_composition() {
    let row = from("function nameRow(doc, session, command)", 40);
    assert!(
        row.contains("formLine(command.send, command.fields, inputs.map((input) => input.value))"),
        "the sidebar's rename field does not compose its line from the frame's own row, so what a \
         person typed is joined to the command by hand and an empty answer is sent as the bare \
         `/name`: {row}"
    );
    assert!(
        row.contains("if (!line) return;"),
        "the rename handler sends what `formLine` refused, so clearing the field asks the terminal \
         what the conversation is called instead of doing nothing: {row}"
    );
    // The field sits inside the row, and the row is the conversation: a press that lands in the
    // field must not open it. For this row that is the conversation already open, so the bubbling
    // press would send `/resume` for the row the name is being typed on.
    assert!(
        row.contains("event.stopPropagation"),
        "a click inside the rename field bubbles to the row, so reaching for the field opens the \
         conversation under the person typing in it: {row}"
    );
    assert!(
        row.contains("sendText(line)"),
        "the rename is not sent on the message route, which is the route every line a person would \
         have typed takes (and the one that reaches the transcript): {row}"
    );
    // The field starts on the label in force, and the page's own `(empty)` is not offered as a name.
    assert!(
        row.contains("session.label === \"(empty)\" ? \"\" : session.label"),
        "the rename field does not start on the conversation's current label, or it would prefill \
         the page's own placeholder for an unnamed conversation and send it: {row}"
    );
    // And the row is drawn because the *frame* described it: which commands a run offers is the
    // frame's fact, so neither the command nor its answers are described a second time here -- which
    // is the rule the destructive rows in this same menu already follow.
    let lookup = from("function frameForm(doc, send)", 14);
    assert!(
        lookup.contains("if (fields.length === 0) continue;"),
        "a command row that says nothing about its answers is treated as a form, so a field appears \
         for a command that never said what it takes: {lookup}"
    );
    assert!(
        from("function sessionMenu(doc, session)", 45)
            .contains("session.current ? frameForm(doc, \"/name\") : null"),
        "the rename field is drawn without asking the frame whether the run offers `/name`, or it is \
         drawn for a conversation other than the open one"
    );
}

/// A destructive row takes two presses, and the second one prints the line it will send.
///
/// §8's last class, and the one where a single press could destroy work that no undo anywhere in
/// flint can bring back. So the row does not send: it opens the candidates the *frame* named a source
/// for (`from: "sessions"` or `"providers"`), drawn from lists the page already holds -- the sidebar's
/// rows and the state frame's providers -- and the candidate row says `<send> <value>`, which is the
/// line that goes when it is pressed. The page never names a command or a number of its own, and the
/// `from` fact is why it does not have to tell `/delete <n|id>` from `/provider rm <name>` by reading
/// them.
#[test]
fn a_destructive_row_opens_its_choices_and_sends_on_the_second_press() {
    let drawn = from("function showCommands(doc)", 175);
    for (needed, why) in [
        ("command.from === \"sessions\"", "the list the frame named, rather than a recognised command"),
        ("command.from === \"providers\"", "the other list, which comes from the state frame"),
        ("doc.confirm = { send: send }", "opening the choices without sending anything"),
        ("sendText(send + \" \" + choice.value)", "the second press sending the frame's `send` plus one candidate"),
    ] {
        assert!(
            drawn.contains(needed),
            "a destructive row is not drawn through `{needed}` ({why}): {drawn}"
        );
    }
    // The first press must not send: the row that opens the choices has no `sendText` in it. Checked
    // at the shape of the branch rather than by eye, because this is the property the whole class
    // exists for.
    let opening = drawn
        .split("doc.confirm = { send: send };")
        .next()
        .expect("the opening press must set the armed state");
    let last_open = opening
        .rfind("row.addEventListener")
        .expect("the row must have a press");
    assert!(
        !opening[last_open..].contains("sendText"),
        "the first press sends: {drawn}"
    );
}

/// A value is a reading only on a report row; anywhere else it is a line typed for you.
///
/// The rows that switch provider and model carry `values` now, and the tempting shortcut was to send
/// every value through `askReport`, which is how `/skills <name>` is read. That route runs a command
/// with the terminal **quiet**: for `/provider llamacpp` that means starting a local engine without
/// printing a word anywhere, and for `/model` it means a change nobody can see. So the class decides
/// the route, exactly as it decides which control is drawn: a `panel` row's value is a read, and any
/// other row's value goes on `/message` like something typed into the composer, where the answer
/// lands in the transcript next to the change. `tests/cli_output.rs` asserts the other half of this —
/// that `/report` refuses `/provider other` — because the page honoring it is not the same thing as
/// the process enforcing it.
#[test]
fn a_switch_value_is_typed_rather_than_read() {
    let offered = from("for (const item of offered) {", 22);
    assert!(
        offered.contains("if (className === \"panel\") askReport(doc, item.line);")
            && offered.contains("else sendText(item.line);"),
        "the route a value takes is not the row's class, so a switch could be run with the terminal \
         quiet: {offered}"
    );
}

/// A row the panel cannot press says where its control is.
///
/// The first person to use the panel asked why some rows press and others do not, and the honest
/// answer was in the CSS: a reference row was deliberately drawn to look exactly like a pressable
/// one, so that no command looked different from the others. That reads well and it does not work —
/// two rows that look the same and behave differently are a puzzle, not a list. The panel still lists
/// every command (a list with holes in it teaches nothing), and a row with no control of its own is
/// now marked and says, on hover, where its control is. The words are the page's own, because where
/// *this page* puts its pickers and switches is the page's business; which commands exist is not.
#[test]
fn a_row_the_panel_cannot_press_says_where_its_control_is() {
    let homes = from("const COMMAND_HOMES = {", 8);
    for (needed, why) in [
        ("button:", "the header's buttons"),
        ("selector:", "the one selector the page cannot offer values for"),
        ("form:", "the terminal, for the ones that ask questions"),
    ] {
        assert!(
            homes.contains(needed),
            "`{needed}` has no home to name ({why}): {homes}"
        );
    }
    let drawn = from("if (offered.length === 0) {", 14);
    assert!(
        drawn.contains("el(\"div\", \"row reference\")"),
        "a row with no control must be drawn as a marked, unpressed row rather than as a button: {drawn}"
    );
    assert!(
        drawn.contains("COMMAND_HOMES[className]"),
        "and it must take its explanation from the class the frame gave: {drawn}"
    );
}

/// A conversation's row carries its own actions, behind one button, and the frame says which.
///
/// Asked for after using the page: deleting a conversation meant finding the command panel, reading
/// `/delete <n|id>` there and picking the number off the sidebar by eye. The row is where the
/// conversation is, so the row is where its actions belong -- and the shape is the panel's, one
/// level down: a button that opens a short list, and a row in that list that prints the whole line
/// it will send (`/delete 3`) before it sends it.
///
/// The list is built from the **frame**, like every other control: the rows the state frame marks
/// `class: "danger"` with `from: "sessions"`. This file does not know the word `/delete`, and a
/// command the terminal gains or loses moves the menu with no edit here.
#[test]
fn a_conversation_row_carries_its_own_actions() {
    let drawn = from("function sessionRow(doc, session)", 80);
    for (needed, why) in [
        ("el(\"button\", \"more\"", "the row has a control of its own"),
        ("doc.menu", "the open menu is document state, not a hidden node"),
        ("destroyingRows(doc, \"sessions\")", "the actions come from the frame, not from a word in this file"),
        (
            "command.send + \" \" + session.n",
            "the line it sends is the frame's `send` and the row's own number",
        ),
        ("nothing to do from here", "an empty menu says so rather than drawing nothing"),
    ] {
        assert!(
            drawn.contains(needed),
            "the row's menu is not drawn through `{needed}` ({why}): {drawn}"
        );
    }
    // One place reads `from` for the sidebar, and it is the same test the panel's choice lists make:
    // a row is an action on another conversation only if it destroys something *and* says where its
    // argument comes from.
    let helper = from("function destroyingRows(doc, from)", 12);
    assert!(
        helper.contains("command.class === \"danger\"") && helper.contains("command.from === from"),
        "the helper must require both the class and the source of the argument: {helper}"
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

/// Where the page was reading is remembered for the next load, and the token never is.
///
/// What this holds is the *wiring* and the policy: that the pair is written where the position is
/// final, that the three cases have words, and that the origin-wide store is not used at all. What the
/// three cases *do* -- a gap, another conversation, a position past the end -- is checked behaviourally
/// by `scripts/web-view-test.js`, which calls the page's own `notePosition` in its Node sandbox; a text
/// assertion here still passes when the branch behind the wording is unreachable, which is what was
/// measured while this was written.
///
/// `ROADMAP.md` section 11 item 8: a reload must rebuild from the file, so what the page can hold
/// honestly is the pair -- the conversation's id and the byte position it had drawn to -- and what it
/// can do with it is say what arrived while it was closed, or that a position does not fit this file.
/// The negative half is the one that needs holding: the token can drive the composer, so a store any
/// document on the loopback origin can read must never hold it, which is why the shared store is
/// forbidden outright rather than merely unused today.
#[test]
fn the_page_remembers_where_it_was_and_never_stores_the_token() {
    let html = view();
    for needle in [
        "sessionStorage",
        "pagehide",
        "\"flint.seen\"",
        "bytes arrived while this page was closed",
        "the file it was reading is not this one",
    ] {
        assert!(
            html.contains(needle),
            "the page no longer says {needle:?}, which is how a reload accounts for what it missed"
        );
    }
    forbidden(
        "localStorage",
        "the pair is this tab's own reading, and a store shared with every other document on the \
         loopback origin is where a token would end up by accident",
    );
    forbidden(
        "sessionStorage.setItem(\"flint.token\"",
        "the token must never be written to storage: it is a credential for a run with no permission layer",
    );
    // The write happens where the position is final, not as frames arrive: a page that stored the
    // load-time cursor would report the whole of a session as "arrived while you were away".
    let hook = from("addEventListener(\"pagehide\"", 4);
    assert!(
        hook.contains("remember(doc.meta && doc.meta.id, applied)"),
        "the remembered position must be the one drawn to: {hook:?}"
    );
}

/// The page draws `/say --to` as a picker over the live runs, and never as a pid to type.
///
/// `ROADMAP.md` section 11 item 9(ii) and section 8: a pid typed into a text field is *prose*, and a
/// message that quietly went to whoever the sentence named is worse than one that reached everybody
/// here. So the frame says the answer comes from a list (`from: "peers"`), the page fetches that list
/// from `GET /peers` when the row is drawn, and the answer is a `select` whose value is the pid and
/// whose label is who that pid is. The behaviour -- the composed line, the broadcast default, the
/// label -- is checked in `scripts/web-view-test.js` against the page's own functions; what is held
/// here is that the wiring exists and that the route is the one the terminal also reads.
#[test]
fn the_page_offers_the_live_runs_a_message_can_address() {
    let html = view();
    for needle in [
        "spec.from === \"peers\"",
        "function peerPicker()",
        "fetch(\"/peers\"",
        "(everyone here)",
        "it waits in the file for the next run",
    ] {
        assert!(
            html.contains(needle),
            "the page no longer offers the runs a message can address: {needle:?} is gone"
        );
    }
    // A hand-written option list would be a second reader of the presence records; the route is the
    // one the terminal's own `/say` is answered from, so the page cannot offer a pid it would not.
    forbidden(
        "scan_in(",
        "the page must read presence through GET /peers rather than deriving it from files itself",
    );
    // A pid is what is sent and who it is that is read, which is why this cannot go through
    // `fillSelect` -- that helper makes the two the same string.
    let filler = from("function fillPeerSelect(select, peers, note)", 12);
    assert!(
        filler.contains("option.value = String(peer.pid)"),
        "the picker must send a pid: {filler:?}"
    );
    assert!(
        filler.contains("everyone.value = \"\""),
        "the broadcast must stay offered as the default: {filler:?}"
    );
}

/// A path can be opened in the program this machine uses for it, and only deliberately.
///
/// The one control on this page that starts a program, which is why it is not the path itself. A
/// path in the transcript stays what it has always been -- a button that reads the file into this
/// page's own panel -- and opening it *outside* the page is a second, named press whose title says
/// what will happen. Two reasons for the split, and neither is taste:
///
/// - The transcript's text is model-written. A plain click on it must never be the thing that
///   launches a process, or reading an answer becomes a way to run what the answer names.
/// - A person who wants to *look* at a file is already served by the panel. `open` is for the
///   cases the panel cannot serve -- a directory, a file too large to preview, a PDF -- which is
///   exactly what `GET /file`'s own refusals say to do.
///
/// What is held here is the wiring: one route, one body, never a navigation. The route's own
/// decisions are held in `src/web.rs`, and the press is driven for real by
/// `scripts/browser-controls-test.js`.
#[test]
fn a_path_opens_outside_the_page_only_through_the_route() {
    let html = view();
    for needle in [
        "\"/open\"",
        "function openBody(path)",
        "function openOutside()",
        "not opened: ",
        "id=\"preview-open\"",
        "open it where it lives",
    ] {
        assert!(
            html.contains(needle),
            "a path can no longer be opened where it lives: {needle:?} is gone"
        );
    }

    // The body is JSON built by a function of its own, for the reason `fileRoute` is one: a path
    // with a quote or a backslash in it has to arrive as itself, and a body assembled from a string
    // concatenation is where a Windows path would become a broken one.
    let body = from("function openBody(path)", 4);
    assert!(
        body.contains("JSON.stringify({") && body.contains("path: path"),
        "the body must be JSON carrying the path: {body:?}"
    );

    // One press, one route: the panel's button is the only caller, and the transcript's path button
    // still reads into the panel. The needle starts inside the quotes on purpose -- the page's own
    // call is `getElementById("preview-open")`, and a needle carrying the function's name is a
    // needle that breaks on the one letter it spells differently from this test's expectation.
    let listeners = from("(\"preview-open\").addEventListener", 2);
    assert!(
        listeners.contains("openOutside()"),
        "the open control must be the second press that opens outside: {listeners:?}"
    );
    assert!(
        html.contains("openPreview(part.path, part.line)"),
        "a click on a path must still be this page's own preview"
    );

    // And it never leaves the page to do it. A navigation would be a page that replaced itself with
    // whatever the model wrote into the transcript.
    forbidden(
        "window.open(",
        "opening a path must go through the route, not through a pop-up",
    );
    forbidden(
        "location.href =",
        "opening a path must not navigate the page away from the run it is showing",
    );
    forbidden(
        "location.assign(",
        "opening a path must not navigate the page away from the run it is showing",
    );
}

/// A readonly run never offers the OS open, and the frame is where that is read.
///
/// `readonly` is all-or-nothing and it refuses to launch programs. That makes this control the one
/// place a page could hand the model something the guard denies it: the model cannot run a program
/// in a readonly run, and a button that launched one when a person clicked it -- on text the model
/// wrote -- would be that program running anyway, one click removed.
///
/// So the page does not draw the control as available, and it does not decide that for itself: the
/// toggle is in the state frame, printed by the terminal and drawn here from the same field. The
/// route refuses regardless, because the page is not the authority -- but a live button that always
/// bounced would be a worse answer than the one that says why before it is pressed.
#[test]
fn a_readonly_run_is_never_offered_an_os_open() {
    let guard = from("function readonlyOn(state)", 10);
    assert!(
        guard.contains("toggles") && guard.contains("\"readonly\""),
        "the guard must be read from the frame's own toggle: {guard:?}"
    );

    let head = from("function paintPreviewHead()", 20);
    assert!(
        head.contains("preview-open") && head.contains("disabled"),
        "the panel's open control must be disabled when the run is readonly: {head:?}"
    );
    assert!(
        head.contains("readonlyOn(doc"),
        "and disabled from the frame's toggle rather than from a second opinion: {head:?}"
    );
    // A run with no page behind it -- a dropped session -- cannot open anything either: there is no
    // route to ask. That is `canSend`, which is the same flag every other route on this page reads.
    assert!(
        head.contains("canSend"),
        "a file-mode page has no run behind it and must not offer the route: {head:?}"
    );
}
