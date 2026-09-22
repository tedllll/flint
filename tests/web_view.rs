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
        html.contains("el(\"button\", \"path\", part.written || part.path)"),
        "a path is a button, which cannot navigate the page"
    );
    // ...and the button *says* what the token said, line number and all: a `grep` hit that reads
    // `src/web.rs:412` in the tool result must not read `src/web.rs` in the button, because where in
    // the file is the one thing a hit is worth reading for. `part.path` is what the route is asked
    // for; `part.written` is what the reader met.
    assert!(
        html.contains("part.line ? part.path + \":\" + part.line : part.path"),
        "and the tooltip names the line the panel will open at"
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

/// A picture is read the same way a file is, and never by putting the token in a URL.
///
/// The picture half of the panel is the one place in this page where a *browser* fetches something
/// rather than the page's own script doing it -- an `<img src="…">` is a request with no headers --
/// so it is exactly where the obvious implementation would have appended `&token=…` and been done.
/// That is refused for a reason that is already written down: the token is accepted in the query
/// string on `/` alone, because a URL lands in a history, a log and a shared link, and the routes
/// that can change something are not the place to accept that. So the page fetches the bytes itself
/// with `authHeader()` and hands the browser a blob URL instead, and this test is what keeps the two
/// halves in that order.
///
/// The other half is the same rule §12 has always had, applied to a second body shape: the picture is
/// served by the run, not composed by the page -- no `data:` image built here, and no markup assigned
/// anywhere.
#[test]
fn a_picture_is_read_with_the_pages_own_auth_and_never_a_token_in_a_url() {
    let html = view();
    assert!(
        html.contains("function imageRoute(path)"),
        "the picture route must be one function, for the same reason the text route is"
    );
    assert!(
        html.contains("\"/image?path=\" + encodeURIComponent(path)"),
        "the picture's path travels percent-encoded, exactly as the text one does"
    );
    assert!(
        html.contains("fetch(imageRoute(preview.path), { headers: authHeader() })"),
        "the page fetches the picture itself, with the header the route accepts"
    );
    assert!(
        html.contains("URL.createObjectURL(await response.blob())"),
        "and hands the browser a blob URL rather than a route with a token on it"
    );
    assert!(
        html.contains("URL.revokeObjectURL(pictureUrl)"),
        "a blob URL holds its bytes until it is let go of, and a session opens many pictures"
    );
    // The token in a URL would look like this, and it must appear nowhere: not in an `img`, not in a
    // fetch. `authHeader` is the only way this page offers a token, and it is a header.
    for needle in ["&token=", "?token=", "img.src = \"/image", "src=\"/image"] {
        // `?token=` is accepted on `/` alone, which is the one place a person pastes a URL: the page
        // itself only ever builds it there, in the address `--web` printed.
        let sites = sites(html, needle);
        assert!(
            sites.is_empty() || (needle == "?token=" && sites.len() == 1),
            "the token belongs in a header: {needle} appears at {sites:?}"
        );
    }
    // And the picture's own frame is text like everything else: the alt text, the note and the
    // title are set from the route's headers, not from anything that could carry markup.
    let frame = from("function showPicture(url, response)", 40);
    assert!(
        !frame.contains("innerHTML") && !frame.contains("insertAdjacentHTML"),
        "the picture is drawn by assignment, not by markup:\n{frame}"
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
///
/// The refusal lives in `readText` rather than in `readPreview`, because the preview now has two
/// answers to one press and the text one is where every refusal is met: a picture's route refuses a
/// file that is not a picture by *falling through* to this, so the sentence a reader sees is always
/// the text route's own. That fall-through is asserted here too, since it is the reason the split
/// exists at all.
#[test]
fn the_preview_shows_the_routes_own_refusal_rather_than_an_empty_panel() {
    let body = from("function paintPreviewRefusal(body, status)", 14);
    assert!(
        body.contains("showPlain(text, String(body == null ? \"\" : body).trim())"),
        "the refusal is shown as it was written:\n{body}"
    );
    assert!(
        body.contains("\"HTTP \" + status"),
        "and the status beside it, for the reader who wants the code:\n{body}"
    );
    let status = from("async function readText()", 30);
    assert!(
        status.contains("paintPreviewRefusal(body, response.status)"),
        "a refused read must hand its sentence and its code to that function:\n{status}"
    );
    let chooser = from("async function readPreview()", 20);
    assert!(
        chooser.contains("if (imageExt(preview.path) && (await readPicture())) return;")
            && chooser.contains("await readText();"),
        "a picture is asked for first and a refusal falls through to the text route:\n{chooser}"
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

/// The settings are the run's own commands, laid out as controls.
///
/// §8's first control, and the reason the read channel came first: a control cannot be drawn without
/// knowing the options, and the page may not read `config.toml` for them -- a second reader of the
/// same state is a second thing that can disagree with the process, which is not hypothetical here
/// (`/config` prints the file's `readonly` beside the value in force, and they differ for a whole
/// class of runs). So the whole of a setting arrives in the `state` frame -- its key, its value, the
/// words a choice takes, how to draw it, what changing it means, and the line that changes it -- and
/// what is checked here is the half that has an answer in the source rather than in a browser: that
/// the page composes the line from the frame's own `send` rather than from a name it knows, and that
/// the controls live in screens the frame's `group` words decide.
///
/// Two things have to hold, and each is a way this can be wrong rather than merely absent:
///
/// * the dialog starts hidden, because a page opened from a dropped file has no process behind it
///   and a settings screen with nothing in it is a promise the page cannot keep;
/// * every control sends exactly one line, and that line is the frame's `send` plus the value or the
///   words typed -- so the terminal and the page cannot come to disagree about what it means;
/// * the `state` frame is applied where it arrives, and not left to fall through to `applyLine`,
///   where a named frame's data would be read as a line of the run's vocabulary and skipped in
///   silence. That failure would look exactly like the feature not being built.
#[test]
fn the_settings_are_the_runs_own_lines_and_nothing_else() {
    let markup = from("<div class=\"settings-body\">", 8);
    assert!(
        markup.contains("id=\"settings-nav\"") && markup.contains("id=\"settings-panes\""),
        "the settings dialog has no rail and no place to draw a screen into: {markup}"
    );

    // Built from the frame, which is the whole rule: the key, the value in force, the words a
    // choice may take, the frame's word for how to draw it, and what changing it means.
    let built = from("function drawSettings(doc, key, box)", 30);
    for (needed, why) in [
        ("state.settings", "the settings the process reports"),
        ("setting.group !== key", "which screen a setting belongs on"),
        ("setting.key", "the name of the setting"),
    ] {
        assert!(
            built.contains(needed),
            "a screen is not drawn from the frame's `{needed}` ({why}): {built}"
        );
    }
    let row = from("function settingRow(doc, setting)", 45);
    for (needed, why) in [
        ("setting.choices", "the words a choice takes, which the page must not carry itself"),
        ("setting.value", "the value in force, which the page must not remember"),
        ("setting.help", "what changing it means"),
        ("setting.kind === \"number\"", "how to draw it, which only the frame knows"),
    ] {
        assert!(
            row.contains(needed),
            "a setting's control is not drawn from `{needed}` ({why}): {row}"
        );
    }

    // What each control sends: one line, composed of the frame's `send` and nothing else. A choice
    // sends `send + " " + value` -- the same composition `/provider <name>` has always been -- and a
    // line typed into a box sends `send + " " + what is in the box`, trimmed so that a stray space
    // cannot make an empty setting look like a change.
    let choice = from("select.addEventListener(\"change\"", 6);
    assert!(
        choice.contains("sendText(send + \" \" + e.target.value)"),
        "a choice must send the frame's own line with the word that was chosen: {choice}"
    );
    assert_eq!(
        choice.matches("sendText(").count(),
        1,
        "a choice sends exactly one thing: {choice}"
    );
    let typed = from("form.addEventListener(\"submit\"", 6);
    assert!(
        typed.contains("const said = send + \" \" + input.value.trim();")
            && typed.contains("sendText(said);"),
        "a typed setting must send the frame's own line with what was typed: {typed}"
    );
    assert_eq!(
        typed.matches("sendText(").count(),
        1,
        "a typed setting sends exactly one thing: {typed}"
    );

    assert!(
        from("if (frame.event === \"state\")", 5).contains("applyState(doc, frame.data)"),
        "the state frame has to be applied where it arrives"
    );
    // And the page's only source for those settings is the frame: no route of its own, and no
    // reading of the file the process owns.
    assert!(
        !view().contains("fetch(\"/config\""),
        "the page must be told the state, not read the config itself"
    );
}

/// A switch is a setting, and neither its name nor the words it takes are the page's.
///
/// §8 again, and the failure mode is specific: a control built from a list the page carries is a
/// control that can offer a word the command refuses, or show a value the run is not on. That list
/// used to be a channel of its own -- `state.toggles`, one row per switch, derived in `src/main.rs`
/// so that the page never had to know `/verbose off|on|full` -- and the settings dialog replaced it:
/// a switch is now one entry of `state.settings`, on the screen the frame files it on, with the words
/// it takes beside it. This test is the other side of that: no second channel is left behind.
#[test]
fn a_switch_is_a_setting_and_not_a_word_the_page_knows() {
    let html = view();
    // One list of switches, and it is the frame's.
    assert!(
        from("function drawSettings(doc, key, box)", 30).contains("state.settings"),
        "the switches are not drawn from the frame's settings"
    );
    assert!(
        !html.contains("state.toggles") && !html.contains("function toggles("),
        "the page still reads a `toggles` field the frame no longer sends, so a switch can show a \
         value the run is not on"
    );
    // And the page holds none of those words: which switches exist is the frame's answer, which is
    // what lets `/thinking` be added to the terminal without touching this file. The needles carry
    // their slash, so that `</details>` is not read as a switch called `/detail`.
    for leaked in ["\"/verbose\"", "\"/detail\"", "\"/readonly\"", "\"/hear-peers\"", "\"/thinking\""] {
        assert!(
            !html.contains(leaked),
            "the page carries `{leaked}` itself, so a switch can offer a word the command refuses"
        );
    }
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

/// The dialog's screens are drawn from the frame, and the page carries none of the list itself.
///
/// §8's read channel, second half over the page's own bytes. Two things are checked, and the second
/// is the one that matters: the rows are built from `state.commands`, and the page contains no
/// command name of its own. A page that knew `/provider key <key>` would be a second copy of the
/// terminal's grammar — it would go on offering a command that was renamed, in a dialog whose whole
/// job is to say what there is.
///
/// The *arrangement* is the other half of this round's change: a row carries the screen the frame
/// filed it on (`command.group`), so the page's own list of screens is only names for places and an
/// order to read them in. `the_page_files_every_settings_screen_the_process_hands_it` below holds
/// that pair together.
#[test]
fn the_command_panel_is_drawn_from_the_frame() {
    // Somewhere to put it: one section per screen, built when the dialog opens, and the rows live
    // inside it. The needle is the builder rather than a pane in the markup, because the screens are
    // built rather than written -- what is written is the rail, the container they go into, and the
    // page's own names for them.
    let built = from("function showSettingsPanes()", 40);
    for (needed, why) in [
        ("settings-nav", "the rail"),
        ("settings-panes", "the container the screens are built into"),
        ("\"fields-\" + key", "the settings a screen holds"),
        ("\"row-list-\" + key", "and the command rows, under them"),
    ] {
        assert!(
            built.contains(needed),
            "a screen is not built from `{needed}` ({why}): {built}"
        );
    }

    // Built from the frame: the list, and which screen each row of it belongs on. The second is what
    // this round added, and it is what lets the dialog be six short screens rather than one column.
    let drawn = from("function drawRows(doc, key, panel, list)", 60);
    for (needed, why) in [
        ("state.commands", "the list of commands"),
        ("command.group === key", "the screen the frame filed the row on"),
    ] {
        assert!(
            drawn.contains(needed),
            "the rows are not drawn from the frame's `{needed}` ({why}): {drawn}"
        );
    }
    // Wide enough to reach the branches that actually draw a label: the form branch comes first, and
    // the rows that say what to type are below it.
    let row = from("function drawCommandRow(doc, key, command, list)", 90);
    for (needed, why) in [
        ("command.label", "what the row says to type"),
        ("command.help", "what the row says it does"),
        ("command.class", "which kind of control the row gets"),
    ] {
        assert!(
            row.contains(needed),
            "a row is not drawn from the frame's `{needed}` ({why}): {row}"
        );
    }

    // And the page holds no copy of the command list. These are the strings that would be in it if it
    // did; the screen *names* are the page's own, and the test below holds those to the process's.
    let html = view();
    for leaked in ["/provider key", "/delete <n|id>", "/reload", "inspect the config"] {
        assert!(
            !html.contains(leaked),
            "the page carries `{leaked}` itself, so it can offer a command the terminal does not \
             have"
        );
    }
}

/// Every screen the process can file a row on is a screen the page has a name for, and no others.
///
/// The page's `SETTINGS_PANES` names the places and the order they read in; the process's
/// `page_group` decides which place a row goes in. Neither file can see the other, and the failure is
/// silent in both directions: a group the page has no name for is a row drawn nowhere (a screen the
/// page cannot name is skipped rather than shown unlabelled), and a screen the process never files
/// anything on is a heading over an empty pane. `tests/cli_output.rs` holds the frame's own
/// vocabulary to `/help`; this is the page's half of the same fact, read out of the two files rather
/// than out of a running process, so a rename on either side fails here instead of in a browser.
#[test]
fn the_page_files_every_settings_screen_the_process_hands_it() {
    // What `page_group` can return: every arm of it is `=> "<word>"`, and the fallback returns `None`
    // before the arms are bound, so the words are the whole vocabulary of screens.
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("src/main.rs is in the checkout");
    let at = main
        .find("fn page_group(")
        .expect("src/main.rs files every row on a screen");
    let body = &main[at..];
    let end = body
        .find("\n}\n")
        .expect("page_group is a top-level function and ends at column zero");
    let mut groups: Vec<String> = Vec::new();
    for arm in body[..end].split("=> \"").skip(1) {
        let word = arm.split('"').next().unwrap_or("");
        if !word.is_empty() {
            groups.push(word.to_string());
        }
    }
    groups.sort();
    groups.dedup();
    assert!(
        groups.len() > 4,
        "page_group files rows on {} screens, which is too few to be the dialog: {groups:?}",
        groups.len()
    );

    // The page's own list, read the same way: one entry per line, each starting with its key.
    let mut keys: Vec<String> = Vec::new();
    for line in from("const SETTINGS_PANES = [", 10).lines().skip(1) {
        let line = line.trim();
        if !line.starts_with("[\"") {
            continue;
        }
        keys.push(line[2..].split('"').next().unwrap_or("").to_string());
    }
    keys.sort();
    assert_eq!(
        keys, groups,
        "the page's screens and the process's groups have drifted apart: a row filed on a screen the \
         page cannot name is a row that is drawn nowhere"
    );
}

/// The action buttons are drawn from the frame too, and send the row's own line.
///
/// §8's first control, and the reason it is first: an action takes no argument, so there is nothing
/// to ask for and nothing to confirm — the whole control is "send this line", which is what the
/// composer already does. What is checked here is the part that can go wrong: the line is the
/// frame's `send`, not a name this page reassembles, and a press that is refused puts the screen back
/// to what is in force rather than leaving a control that quietly did nothing.
#[test]
fn the_action_buttons_send_the_frames_own_line() {
    let drawn = from("if (className === \"button\" && send) {", 14);
    for (needed, why) in [
        ("el(\"button\", \"row action\")", "an action drawn as the control it is"),
        ("sendText(send)", "the line, taken from the frame rather than rebuilt here"),
        ("showState(doc)", "putting the screen back when the send is refused"),
        ("command.label", "the button's own words"),
    ] {
        assert!(
            drawn.contains(needed),
            "the buttons are not drawn from `{needed}` ({why}): {drawn}"
        );
    }
    // The tooltip is the frame's help line, which the row already carries: a page that shortened it
    // would be inventing a second name for a command.
    assert!(
        drawn.contains("row.title = help"),
        "a button says what it does with the frame's own help: {drawn}"
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
    let built = from("function drawCommandRow(doc, key, command, list)", 200);
    for (needed, why) in [
        ("className === \"panel\"", "the class that makes a row readable"),
        ("command.send", "the line to ask for, taken from the frame rather than rebuilt here"),
        ("askReport(doc, item.line, key)", "asking the process for the line the row offers, on the screen it is on"),
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
    // exists to prevent. The screen travels with the request because the answer belongs where the row
    // was pressed, and a frame arriving in between must not move it under another heading.
    let asked = from("async function askReport(doc, input, key)", 30);
    for (needed, why) in [
        ("fetch(\"/report\"", "the route that answers without printing"),
        ("messageBody(input)", "the line, in the same body shape the composer sends"),
        ("doc.reading = input", "the screen showing what is being read"),
        ("doc.readingGroup = key", "which screen that reading belongs to"),
        ("paintSettings(doc)", "redrawing the dialog rather than the transcript"),
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

    // And the answer: a `command` frame marked as a panel's fills the screen, and the unmarked shape
    // -- what a typed command produces -- still becomes a transcript block.
    let handled = from("case \"command\":", 20);
    for (needed, why) in [
        ("ev.panel === true", "the mark that says this answer belongs in the dialog"),
        ("doc.readingText", "filling the reading rather than appending to it"),
        ("paintSettings(doc)", "redrawing the dialog"),
    ] {
        assert!(
            handled.contains(needed),
            "a panel's answer is not put in the dialog through `{needed}` ({why}): {handled}"
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
    let drawn = from("function drawCommandRow(doc, key, command, list)", 130);
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
    let drawn = from("function drawCommandRow(doc, key, command, list)", 175);
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
        offered.contains("if (className === \"panel\") askReport(doc, item.line, key);")
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
    let homes = from("const COMMAND_HOMES = {", 6);
    for (needed, why) in [
        ("selector:", "the one selector the page cannot offer values for"),
        ("form:", "the terminal, for the ones that ask questions"),
    ] {
        assert!(
            homes.contains(needed),
            "`{needed}` has no home to name ({why}): {homes}"
        );
    }
    // `button` and `panel` are absent on purpose: both are controls in the dialog now -- an action is
    // a button on its screen, and a report is a row that is read -- so no row can ask for a home.
    assert!(
        !homes.contains("button:") && !homes.contains("panel:"),
        "a class that has its own control must not name a home somewhere else: {homes}"
    );
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

/// The run's settings live in a dialog, and the header keeps one door onto them.
///
/// Asked for after using the page: *the controls are laid out raw on the surface, and they should be
/// in settings* — modelled on DSH, where a settings seat holds what changes the run and the header
/// holds what identifies it. The reason this is a *policy* test rather than a drawing check is that
/// the failure mode is a slow one: a row added straight back into the header, or a control left
/// behind outside the dialog, is invisible in a diff and only shows up as a header that has grown
/// back into the reading column. So the header's own slice of the markup is asserted to hold the
/// name, the work, and the door — and nothing else.
#[test]
fn the_runs_controls_live_in_a_dialog_and_the_header_keeps_one_door() {
    let html = view();
    // The header's top line, as bytes: from the name to the end of its own div.
    let head = from("<div class=\"head-line\">", 40);
    let head = head.split("<!-- The settings dialog").next().unwrap_or(&head);
    for needed in ["id=\"title\"", "id=\"jobs\"", "id=\"settings-open\""] {
        assert!(
            head.contains(needed),
            "the header's line must still carry {needed}: {head}"
        );
    }
    // The controls that used to be laid out on this line, none of which may come back: the two
    // pickers, the switches, the one-press actions, and the command list. `id="meta"` is on the list
    // for the other reason -- the facts about the conversation belong in the dialog's foot, not
    // beside the name.
    for moved in [
        "id=\"pick-provider\"",
        "id=\"pick-model\"",
        "id=\"toggles\"",
        "id=\"actions\"",
        "id=\"meta\"",
        "id=\"command-list\"",
    ] {
        assert!(
            !head.contains(moved),
            "{moved} is back in the header's line, which is the surface this change emptied: {head}"
        );
    }
    // The screens are built rather than written, so what the markup must carry is the rail and the
    // container they are built into -- inside the dialog, which is checked below.

    // The door is a door, and it is shut until the page has a run to describe -- the same rule the
    // controls themselves followed, because a dialog with nothing in it is worse than no dialog.
    let door = from("id=\"settings-open\"", 1);
    assert!(
        door.contains("aria-haspopup=\"dialog\""),
        "the door must say what it opens: {door}"
    );
    assert!(
        door.contains("hidden"),
        "the door ships closed, like the controls it replaced: {door}"
    );
    let shown = from("const door = document.getElementById(\"settings-open\");", 2);
    assert!(
        shown.contains("door.hidden = !state"),
        "the door is offered exactly when the frame describes a run: {shown}"
    );

    // The dialog says what it is -- the attributes are the difference between a modal and a panel
    // that happens to be on screen -- and everything that changes state is *inside* it.
    let dialog = from("id=\"settings\" role=\"dialog\"", 1);
    assert!(
        dialog.contains("aria-modal=\"true\"") && dialog.contains("aria-labelledby=\"settings-title\""),
        "the dialog must be a labelled modal: {dialog}"
    );
    assert!(
        from("<h2 id=\"settings-title\">", 1).contains("settings"),
        "the label the dialog points at must be the dialog's own heading"
    );
    let dialog_at = html
        .find("id=\"settings\" role=\"dialog\"")
        .expect("the dialog is in the page");
    for inside in [
        "id=\"settings-nav\"",
        "id=\"settings-panes\"",
        "id=\"settings-close\"",
        "id=\"meta\"",
    ] {
        let at = html
            .find(inside)
            .unwrap_or_else(|| panic!("the page never draws {inside}"));
        assert!(
            at > dialog_at,
            "{inside} is outside the dialog, so it is still on the surface"
        );
    }

    // Three doors, because a modal with one way out is a trap: its own button, the mask, and Escape.
    let wiring = from("The settings dialog's three doors", 24);
    for (needed, why) in [
        ("settingsDoor.addEventListener(\"click\"", "the door opens it"),
        ("settingsClose.addEventListener(\"click\"", "its own button closes it"),
        ("settingsMask.addEventListener(\"click\"", "a press outside closes it"),
        ("dismissTopmost()", "and Escape follows one order rather than three listeners"),
    ] {
        assert!(
            wiring.contains(needed),
            "the dialog is missing a way out through {needed} ({why}): {wiring}"
        );
    }
    let order = from("function dismissTopmost()", 16);
    let settings_at = order
        .find("settingsOpen()")
        .expect("the modal must be asked about first");
    let preview_at = order.find("preview").expect("the panel is next");
    assert!(
        settings_at < preview_at,
        "the modal is over the panel, so one Escape must close it and leave the panel alone: {order}"
    );

    // One screen at a time, and a screen is the page's own word for a *place it put things* -- not a
    // command name, which is the frame's. The check is that the rail's list is the only list: a second
    // hard-coded screen somewhere would be a pane nothing can reach. Which of them the frame can file
    // a row on is `the_page_files_every_settings_screen_the_process_hands_it`; here it is the shape.
    let rail = from("const SETTINGS_PANES = [", 8);
    for (key, why) in [
        ("[\"model\"", "the endpoint and the model"),
        ("[\"run\"", "how the run behaves"),
        ("[\"limits\"", "what it may spend"),
        ("[\"tools\"", "what it can use"),
        ("[\"conversation\"", "this conversation"),
        ("[\"work\"", "the work it left running"),
    ] {
        assert!(
            rail.contains(key),
            "the rail must offer {key} ({why}): {rail}"
        );
    }
    let panes = from("function showSettingsPane(name)", 14);
    assert!(
        panes.contains("pane.hidden = known !== name"),
        "opening one section must shut the others -- a column of every setting is what this replaced: {panes}"
    );
    let unknown = from("function showSettingsPane(name)", 2);
    assert!(
        unknown.contains("SETTINGS_PANES.some"),
        "a section the rail does not offer must leave the dialog as it was: {unknown}"
    );
}

/// The `/` menu: a launcher in the composer, drawn from the frame, and honest about what it may do.
///
/// Three things are checked, and each is a decision rather than a feature. The menu exists only while
/// the line *is* a command (`menuQuery`), so it cannot cover a sentence being written. Its rows are
/// the frame's, so the page still holds no command name of its own. And what a row does is decided by
/// its class -- with the one rule that matters held to the letter: a `form` row must never write the
/// line, because the line is sent to the run and written into the session file, and a credential in
/// the composer is a credential on disk.
#[test]
fn the_slash_menu_is_a_launcher_drawn_from_the_frame() {
    // Somewhere to put it, and shut until there is something to offer: the composer owns it, so it is
    // inside the form and moves with the box it annotates.
    let html = view();
    let composer = html
        .find("id=\"composer\"")
        .expect("the page must have a composer");
    let menu_at = html.find("id=\"menu\"").expect("the composer has no menu");
    let form_end = html[composer..]
        .find("</form>")
        .map(|at| composer + at)
        .expect("the composer must be a form");
    assert!(
        composer < menu_at && menu_at < form_end,
        "the menu is the composer's own furniture, so it belongs inside the form"
    );
    let markup = from("<div class=\"menu\" id=\"menu\"", 2);
    assert!(
        markup.contains("hidden") && markup.contains("role=\"listbox\""),
        "the menu ships shut and says what it is: {markup}"
    );

    // Open only while the line is a command: a slash that starts the value, and no space yet. Both
    // halves are asserted, because either one alone would be the defect -- a menu that opened
    // mid-sentence, or one that stayed open over the line being written.
    let query = from("function menuQuery(value)", 8);
    assert!(
        query.contains("startsWith(\"/\")"),
        "the menu is for a line that starts with a slash: {query}"
    );
    assert!(
        query.contains("/\\s/.test(rest) ? null") || query.contains("\\s"),
        "and it must close as soon as the line has a space in it: {query}"
    );

    // Drawn from the frame, like the panel: the same `state.commands`, the same row fields, and the
    // page's own group names as the only thing it knows by heart.
    let rows = from("function menuRows(doc, query)", 16);
    assert!(
        rows.contains("state.commands"),
        "the menu must be drawn from the frame rather than from a list of its own: {rows}"
    );
    let built = from("function menuButton(doc, row, at)", 14);
    for (needed, why) in [
        ("row.send", "the line the row stands for"),
        ("row.label", "what the row says it is"),
        ("row.help", "what the row says it does"),
    ] {
        assert!(
            built.contains(needed),
            "the menu's rows are not drawn from the frame's `{needed}` ({why}): {built}"
        );
    }
    for leaked in ["/provider key", "/delete <n|id>", "/reload", "inspect the config"] {
        assert!(
            !html.contains(leaked),
            "the menu carries `{leaked}` itself, so it can offer a command the terminal does not have"
        );
    }

    // What a row commits to, and the rule: the `dialog` dispatch may not touch the line. The slice is
    // taken from the dispatch branch's own marker to the branch after it, so a `setComposerText` that
    // appeared anywhere inside it would be found -- which is the assertion, since a form row is
    // exactly the case where writing the line would put a secret in the transcript.
    let dispatch = from("function menuDispatch(row)", 14);
    assert!(
        dispatch.contains("=== \"form\") return \"dialog\"")
            && dispatch.contains("=== \"panel\") return \"report\""),
        "every class must have its own answer, and a form's is the dialog: {dispatch}"
    );
    // The one rule, held to the letter: the dialog branch may not *complete* the line. The composer's
    // text is sent to the run and written into the session file, so a form command completed into the
    // box would be a credential on disk. It clears the query it was taken from instead, and that
    // distinction is the whole assertion -- a check for the mere absence of `setComposerText` was the
    // first version of this test, and it passed while a real browser showed the query left behind and
    // one Enter away from being sent.
    let branch = from("if (dispatch === \"dialog\") {", 13);
    assert!(
        !branch.contains("setComposerText(send") && !branch.contains("setComposerText(row"),
        "a form row must never complete the line: {branch}"
    );
    assert!(
        branch.contains("setComposerText(\"\")"),
        "and it must clear the query, or the next Enter sends the command the dialog was opened for: {branch}"
    );
    let report = from("if (dispatch === \"report\") {", 18);
    assert!(
        report.contains("setComposerText(\"\")") && !report.contains("setComposerText(send"),
        "a report row clears the query for the same reason -- it was read, not sent: {report}"
    );
    let opens = report.find("openSettings()").expect("a report's reading is shown");
    let asks = report.find("askReport(").expect("and asked for");
    assert!(
        opens < asks,
        "the dialog is opened as the reading is asked for, not after it answers -- otherwise the \
         reader watches the composer for a round trip with nothing saying anything is happening: {report}"
    );
    // Every other class completes the line and nothing more: a keystroke in a menu must not decide
    // anything. So this function holds no `fetch` of its own -- the one thing it can send goes through
    // `askReport`, which owns the `/report` route.
    let take = from("async function takeMenuRow(doc, row)", 60);
    let sends = sites(&take, "fetch(");
    assert!(
        sends.is_empty() && take.contains("askReport(doc, send, screenOf(command))"),
        "the menu may only send what a report row asks for, and only through the report's own \
         function: {sends:?}"
    );
    // And a row's own screen is what the dialog opens at, in both hand-offs: the reading belongs
    // where the row was pressed, and the row a form was taken from has to be findable where the
    // person is now looking.
    assert_eq!(
        take.matches("showSettingsPane(screenOf(command))").count(),
        2,
        "both hand-offs must open the screen the frame filed the row on: {take}"
    );

    // The arrows are the reason this is a menu, and `Escape` is the page's one order: the menu is in
    // front of everything because it is where the keyboard already is.
    let wiring = from("message.addEventListener(\"keydown\"", 14);
    for (needed, why) in [
        ("menuOpen()", "the menu only takes the keys while it is open"),
        ("ArrowDown", "the arrow keys move the mark"),
        ("ArrowUp", "both of them"),
        ("takeMenuRow", "and Enter takes the row"),
    ] {
        assert!(
            wiring.contains(needed),
            "the composer's keys are not wired to the menu's `{needed}` ({why}): {wiring}"
        );
    }
    let order = from("function dismissTopmost()", 12);
    let menu_at = order.find("menuOpen()").expect("the menu must be asked about");
    let settings_at = order.find("settingsOpen()").expect("so must the dialog");
    assert!(
        menu_at < settings_at,
        "one Escape puts away the thing in front, and the menu is inside the composer: {order}"
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
/// The preview reads a Markdown file rather than only showing it, and the reading is the page's own.
///
/// What is checked here is the part a reader of this file can hold still: the two containers, the switch
/// between them, the rule that a *line* opens the source, and the shape of the renderer -- a pure
/// function for the reading, a painter that only ever sets text. What the reading *is* (headings, fences,
/// nested lists, tables, and inline syntax deliberately left alone) is checked by
/// `scripts/web-view-test.js`, which runs `markdownBlocks` under Node; a byte-scan of this page cannot
/// show that a fence is read as a fence.
#[test]
fn a_markdown_file_is_read_not_just_shown() {
    let html = view();
    // The rendered view is a container of its own and it ships shut: the panel's first paint must not
    // depend on a file having been read, and a `div` (rather than reusing the `pre`) is what lets a
    // heading be a heading.
    assert!(
        html.contains(r#"<div class="md" id="preview-md" hidden></div>"#),
        "the rendered container is missing, or does not ship hidden"
    );
    assert!(
        html.contains(r#"<button id="preview-render" type="button" hidden></button>"#),
        "the switch is missing, or does not ship hidden"
    );
    // The reading is lines in, blocks out -- no DOM in it, which is the whole reason the Node harness can
    // ask it what a fence is.
    let parser = from("function markdownBlocks(text)", 60);
    assert!(
        !parser.contains("document.") && !parser.contains("getElementById"),
        "the reading must be a function of its text alone: {parser}"
    );
    assert!(
        parser.contains(r#"kind: "code""#),
        "a fence must be read as code: {parser}"
    );
    // The painter builds nodes and sets text. The page-wide rule against assigning markup is checked
    // elsewhere; what matters here is that this function goes through `el`, which sets `textContent`.
    let painter = from("function markdownNode(block)", 30);
    assert!(
        painter.contains("el(") && !painter.contains("innerHTML") && !painter.contains("outerHTML"),
        "the painter must build nodes rather than markup: {painter}"
    );
    // A line opens the source. This is the one rule that keeps the rendered view from being a *replacement*
    // for the file: a `grep` hit or a compiler error names a line, and a line has no meaning in a reading.
    let view_rule = from("function previewView(path, line)", 4);
    assert!(
        view_rule.contains("isMarkdown(path)") && view_rule.contains("!line")
            && view_rule.contains(r#"? "rendered" : "source";"#),
        "a Markdown file opened at a line must open raw: {view_rule}"
    );
    // What the switch says: which view is showing, on the button *and* in the note. Two places on
    // purpose -- the button is what a person presses, the note is what they check when the reading looks
    // wrong -- and the `hidden` above is what keeps a file with no choice from being offered one.
    let drawn = from("function paintPreviewView(body, response, view, path, line)", 40);
    for (needed, why) in [
        (r#"button.textContent = showing ? "source" : "rendered""#, "the button names the view it would show"),
        (r#"button.hidden = !markdown"#, "a file that is not Markdown is offered no switch"),
        (r#"if (text) text.hidden = showing"#, "only one of the two containers is ever showing"),
        (r#"paintMarkdown(rendered, markdownBlocks(body))"#, "the reading is what the painter is handed"),
        (" · rendered", "the note says which reading is on screen"),
    ] {
        assert!(
            drawn.contains(needed),
            "the preview does not draw `{needed}` ({why}): {drawn}"
        );
    }
    // The bytes are read once and drawn twice: the switch re-reads rather than keeping a copy, because a
    // second copy of a file in the page is exactly the derived state this project does not keep.
    let flip = from(r#"document.getElementById("preview-render").addEventListener"#, 6);
    assert!(
        flip.contains("preview.view = preview.view === \"rendered\" ? \"source\" : \"rendered\"")
            && flip.contains("readPreview()"),
        "the switch must flip the view and read again: {flip}"
    );
    // A refusal is not a file: the route's sentence is the answer, and the switch goes with the reading
    // that did not happen.
    let refusal = from("function paintPreviewRefusal(body, status)", 16);
    assert!(
        refusal.contains("button.hidden = true") && refusal.contains("rendered.hidden = true"),
        "a refusal must take the reading and its switch away: {refusal}"
    );
}

/// The panel numbers the lines of a text file, and the number is a gutter rather than part of the text.
///
/// The *structure* is checked in `scripts/web-view-test.js` (which lines a file has, what a number says,
/// where a wrapped line's continuation goes) and the geometry in `scripts/browser-controls-test.js`
/// (measured, because a number's width is a layout fact). What is held still here is the set of
/// decisions a reader of this page can check in the bytes: the number is not selectable, it is not read
/// out, a *sentence* is never numbered, and where the panel scrolls is a row's own position rather than
/// arithmetic over a line-height.
#[test]
fn a_text_files_lines_are_numbered_beside_the_text_rather_than_in_it() {
    // The gutter is unselectable and the width is a `width` rather than a `min-width`: a minimum lets
    // the box grow with its own digits, so `300` would push the code right on the last row of a long
    // file and nowhere else -- the one thing a shared column may not do.
    let css = from("#preview-text .ln {", 4);
    for (needed, why) in [
        ("user-select: none", "a copied block must not come out with the number in front of it"),
        ("width: 4ch", "the gutter is one width for every row, not one per number"),
        ("text-align: right", "the digits line up on their last place, like every other gutter"),
    ] {
        assert!(css.contains(needed), "the gutter does not say `{needed}` ({why}): {css}");
    }
    assert!(
        !css.contains("min-width"),
        "a minimum width lets a longer number widen its own row: {css}"
    );
    // The lines themselves: one row per line, the number hidden from a screen reader (which should read
    // the code), and the class that widens the gutter for a file whose numbers need the room.
    let lines = from("function showLines(pre, body)", 20);
    for (needed, why) in [
        (r#"el("span", "ln", String(at + 1))"#, "the number is the line's own place in the file"),
        (r#"number.setAttribute("aria-hidden", "true")"#, "the number is furniture, not text"),
        (r#"el("span", "code", line)"#, "the line's text is its own node, which is what a copy takes"),
        ("lines-", "a wider gutter is the container's decision, so every row shares it"),
        (r#"lines[lines.length - 1] === """#, "a trailing break ends the last line"),
    ] {
        assert!(lines.contains(needed), "`showLines` does not say `{needed}` ({why}): {lines}");
    }
    // A refusal, a message, an empty panel: a sentence where the lines would be, and no gutter beside it
    // -- a sentence with `1 2 3` down its left is a sentence pretending to be three lines of a file.
    let plain = from("function showPlain(pre, message)", 6);
    assert!(
        plain.contains("while (pre.firstChild) pre.removeChild(pre.firstChild)")
            && plain.contains(r#"pre.className = """#),
        "a sentence must clear the lines and the width they needed: {plain}"
    );
    // The painter hands the bytes to `showLines` rather than assigning the container's text: the panel's
    // own `textContent` is the numbers run together with the code, so a test that read it as the file
    // would be reading something a person never sees.
    let painted = from("function paintPreviewView(body, response, view, path, line)", 40);
    assert!(
        painted.contains("showLines(text, body)") && !painted.contains("text.textContent = body"),
        "the raw view must be drawn as numbered lines: {painted}"
    );
    // Where a `grep` hit's line puts the scroll: the row, measured, rather than `(line - 1)` times a
    // line-height -- which is only right while every line above the target is one visual line tall.
    let scroll = from("function scrollToLine(text, line)", 12);
    assert!(
        scroll.contains("rows[line - 1]") && scroll.contains("row.offsetTop - text.offsetTop"),
        "the scroll must land on the line's own row: {scroll}"
    );
}

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
/// guard is a setting in the state frame, printed by the terminal and drawn here from the same field.
/// The route refuses regardless, because the page is not the authority -- but a live button that
/// always bounced would be a worse answer than the one that says why before it is pressed.
#[test]
fn a_readonly_run_is_never_offered_an_os_open() {
    let guard = from("function readonlyOn(state)", 10);
    assert!(
        guard.contains("state.settings") && guard.contains("\"readonly\""),
        "the guard must be read from the frame's own settings: {guard:?}"
    );

    let head = from("function paintPreviewHead()", 20);
    assert!(
        head.contains("preview-open") && head.contains("disabled"),
        "the panel's open control must be disabled when the run is readonly: {head:?}"
    );
    assert!(
        head.contains("readonlyOn(doc"),
        "and disabled from the frame's own setting rather than from a second opinion: {head:?}"
    );
    // A run with no page behind it -- a dropped session -- cannot open anything either: there is no
    // route to ask. That is `canSend`, which is the same flag every other route on this page reads.
    assert!(
        head.contains("canSend"),
        "a file-mode page has no run behind it and must not offer the route: {head:?}"
    );
}
