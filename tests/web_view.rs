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

/// The answers of a form become one line, and a missing one stops it.
///
/// The half a drawing cannot show. `send` with no answers is the command's *interactive* form --
/// `/provider add` bare is a wizard that asks a person for five things one at a time -- so a page that
/// sent a partly-filled form would hand the run to a wizard whose only reader is a terminal nobody is
/// sitting at. Which answers are required is the frame's word (`optional`), not the page's guess from
/// the label, for the same reason the field's kind is: a page that decided would be re-deriving this
/// program's grammar.
#[test]
fn the_answers_of_a_form_become_one_line_and_a_missing_one_stops_it() {
    let answers = from("function formLine(send, fields, values)", 20);
    assert!(
        answers.contains("if (spec.optional) break;"),
        "an empty optional answer does not end the line, so the answer after it is promoted into its \
         place: {answers}"
    );
    assert!(
        answers.contains("if (!answer) {\n      if (spec.optional) break;\n      return null;\n    }"),
        "a required answer that is empty does not stop the line, so the bare command is sent: \
         {answers}"
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
