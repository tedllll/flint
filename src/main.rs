//! flint — a minimal cross-platform command-line agent.
//!
//! Modes:
//!   flint                          interactive REPL
//!   flint -p "..."                 one-shot prompt
//!   flint --continue               resume the most recent session
//!   flint exec "npm i -g ..."      run a command directly, no model involved
//!   flint --list-sessions          show past sessions
//!
//! The `exec` mode is the last line of defence: when every provider is
//! unreachable, flint still runs commands.

use flint::{agent, config, context, display, event, ndjson, provider, search, session, term, tools, web};

use anyhow::{anyhow, Context, Result};
use display::{Printer, BOLD, CHATTY, DIM, GREEN, NORMAL, QUIET, RED, RESET, YELLOW};
#[allow(unused_imports)]
use display::Palette;
use event::Event;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use term::Term;

#[derive(Default)]
struct Args {
    prompt: Option<String>,
    continue_last: bool,
    /// A specific session to resume: its list index, an id or id prefix, or a path.
    ///
    /// `--continue` only ever reaches the most recent one, which is useless when the
    /// session worth returning to is three conversations back -- and going back is the
    /// normal case when the last thing you did was break something.
    resume: Option<String>,
    /// A name for the conversation this run writes to.
    ///
    /// Applied after the session exists, as an appended `title` line: a run that resumes
    /// can be renamed as easily as a new one, and neither requires rewriting the file.
    name: Option<String>,
    /// Move a session into the archive and exit. A list number, an id prefix, or a path.
    archive: Option<String>,
    /// Delete a session and exit.
    delete: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    readonly: bool,
    no_color: bool,
    /// Write the run as NDJSON on stdout instead of prose, for a program to read.
    ///
    /// Only meaningful with a prompt: an interactive session has no stream to write, and
    /// `exec` is plain by contract because its output is the child's own bytes.
    json: bool,
    list_sessions: bool,
    exec: Option<String>,
    /// Serve a browser view of *this* run on loopback.
    ///
    /// A window and not a mode (`docs/web-mode.md` §1): flint starts exactly as it does
    /// without this, and additionally answers a browser. The terminal keeps working, the
    /// process stays the only writer of the session file, and closing the tab loses nothing.
    web: bool,
    /// The port for that listener. Zero -- the default -- means "whatever is free".
    ///
    /// Ephemeral by default because a fixed port collides with whatever else is on loopback
    /// (a local model server on 11434, 1234 or 8080, most often) and the URL has to be
    /// printed anyway, so making it guessable buys nothing.
    port: Option<u16>,
    /// `debug <what>`, the words after the subcommand.
    ///
    /// A namespace rather than a flag per question -- `--debug-prompt-input` would be one
    /// option per thing anyone ever wants to look at, and the next question is always about
    /// something else. Nothing here sends anything or writes a session.
    debug: Option<Vec<String>>,
    help: bool,
    cwd: Option<String>,
}

/// Whether ANSI colour may be emitted.
///
/// Honours `--no-color` and `NO_COLOR`, and gives up automatically when stdout is
/// not a terminal: escape codes in a pipeline are noise that breaks
/// `flint --help | less` and `| grep`.
///
/// Called from `main` as well as `real_main`, because the top-level error handler
/// prints before `real_main` can report anything -- hardcoding colour there leaks
/// escapes into every redirected failure.
fn colour_allowed(no_color: bool) -> bool {
    !no_color && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

fn main() {
    // Set up the async runtime by hand: it keeps `#[tokio::main]` out of the
    // way and lets us return a normal exit code from any failure.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("flint: cannot start async runtime: {e}");
            std::process::exit(1);
        }
    };

    // The only thing needed to report a startup failure in the right colours.
    let color = colour_allowed(std::env::args().any(|a| a == "--no-color"));

    let code = match runtime.block_on(real_main()) {
        Ok(code) => code,
        Err(e) => {
            let (red, reset) = if color { ("\x1b[31m", "\x1b[0m") } else { ("", "") };
            eprintln!("{red}flint: error:{reset} {e:#}");
            1
        }
    };
    std::process::exit(code);
}

/// Turn what the user typed after `--resume` into a session file.
///
/// Three forms, in the order they are tried, because each is the natural way to name a
/// session in a different situation: the number from `/sessions` when you are looking at
/// the list, an id prefix when you have seen the id, and a path when you have the file.
/// Guessing wrong must never silently open the wrong conversation, so every ambiguous or
/// missing case is an error that says what the options were.
fn resolve_session(target: &str) -> Result<PathBuf> {
    let dir = config::sessions_dir();

    if let Ok(index) = target.parse::<usize>() {
        let sessions = session::list(&dir)?;
        if index == 0 || index > sessions.len() {
            return Err(anyhow!(
                "no session {index}: /sessions lists {} (1 is the most recent)",
                sessions.len()
            ));
        }
        return Ok(dir.join(format!("{}.jsonl", sessions[index - 1].0)));
    }

    let prefix = target.trim_end_matches(".jsonl");
    // The archive is searched by id as well as the live directory: a conversation that
    // was filed away is still something you may want to read, and `mv` by hand must not
    // make it unreachable. It is not offered by number, because the numbers are the ones
    // `/sessions` prints and that list does not include the archive.
    let mut candidates: Vec<(String, PathBuf)> = session::list(&dir)?
        .into_iter()
        .map(|(id, _)| (id.clone(), dir.join(format!("{id}.jsonl"))))
        .collect();
    let archived = session::archive_dir(&dir);
    candidates.extend(
        session::list(&archived)?
            .into_iter()
            .map(|(id, _)| (id.clone(), archived.join(format!("{id}.jsonl")))),
    );
    let matches: Vec<(String, PathBuf)> = candidates
        .into_iter()
        .filter(|(id, _)| id.starts_with(prefix))
        .collect();
    match matches.len() {
        1 => return Ok(matches[0].1.clone()),
        0 => {}
        _ => {
            let names: Vec<&str> = matches.iter().map(|(id, _)| id.as_str()).collect();
            return Err(anyhow!(
                "'{target}' matches {} sessions: {}. Use a longer prefix.",
                matches.len(),
                names.join(", ")
            ));
        }
    }

    let path = PathBuf::from(target);
    if path.is_file() {
        return Ok(path);
    }
    Err(anyhow!(
        "no session matches '{target}'. Run `flint --list-sessions` or `/sessions`."
    ))
}

/// Replay a loaded conversation onto the screen.
///
/// Short on purpose. The point is to show that the history arrived and which
/// conversation it is -- the last few exchanges do that, and scrolling a hundred
/// messages of old transcript is worse than useless when the session file is right
/// there.
fn print_transcript(history: &[event::Message], printer: &Printer<'_>) {
    const TAIL: usize = 12;
    let shown: Vec<&event::Message> = history
        .iter()
        .filter(|m| !matches!(m, event::Message::System { .. }))
        .collect();
    let start = shown.len().saturating_sub(TAIL);
    printer.term().blank();
    printer.term().line(format_args!(
        "{}",
        printer.dim(&format!(
            "\u{2500}\u{2500} {}{} \u{2500}\u{2500}",
            if start > 0 {
                format!("— {start} earlier messages, ")
            } else {
                String::new()
            },
            "resumed transcript"
        ))
    ));
    for message in &shown[start..] {
        match message {
            event::Message::User { content } => {
                printer
                    .term()
                    .line(format_args!("{} {}", printer.style(BOLD, ">"), content));
            }
            event::Message::Assistant { content, tool_calls, .. } => {
                let text = content.as_deref().unwrap_or("").trim();
                if !text.is_empty() {
                    printer.term().line(format_args!("{text}"));
                }
                for call in tool_calls {
                    printer.term().line(format_args!(
                        "{}",
                        printer.dim(&format!("  \u{2713} {}", call.name))
                    ));
                }
            }
            event::Message::Tool { content, .. } => {
                // One line: the stored output can be a whole file.
                let first = content.lines().next().unwrap_or("").trim();
                let first: String = first.chars().take(100).collect();
                printer
                    .term()
                    .line(format_args!("{}", printer.dim(&format!("  \u{2502} {first}"))));
            }
            event::Message::System { .. } => {}
        }
    }
    printer.term().blank();
}

async fn real_main() -> Result<i32> {
    let args = parse_args(std::env::args().skip(1).collect())?;

    // Decide colour before anything prints.
    let color = colour_allowed(args.no_color);

    if args.help {
        print_help(color, &Term::plain());
        return Ok(0);
    }

    let cwd = match &args.cwd {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().context("cannot determine working directory")?,
    };

    // ---- exec: no model, no network, and deliberately no config. ----
    //
    // This is the last line of defence: when every provider is unreachable this
    // must still run a command, so it must not depend on config loading (which
    // could fail, or create a config file as a side effect of `exec echo hi`).
    //
    // Plain output: exec is for scripts, and its contract is the child's own
    // bytes plus its exit code.
    if let Some(command) = &args.exec {
        // `load_existing`, not `load`: no config is created, nothing is printed, and a
        // config that cannot be parsed does not stop the command from running. The old
        // call created a config file and printed "set your API key ... then re-run"
        // *before* running the command anyway -- untrue, since exec needs no key, and
        // the one message guaranteed to make someone stop and go debugging.
        let cfg = config::Config::load_existing();
        return exec_direct(&cfg, command, &cwd, &Term::plain()).await;
    }

    // Everything below may need a provider, so a config is required from here.
    let mut cfg = config::Config::load()?;


    // ---- list sessions ----
    if args.list_sessions {
        let sessions = session::list(&config::sessions_dir())?;
        if sessions.is_empty() {
            println!("(no sessions yet)");
        }
        // Plain output on purpose: this mode is for scripts, and its contract is
        // one session per line. The *first* field is still the id -- the session
        // is the id, exactly as before -- and the number in front is what
        // `--resume N` takes.
        for (index, (id, summary)) in sessions.iter().enumerate() {
            println!("{}  {id}  {summary}", index + 1);
        }
        return Ok(0);
    }

    if cfg.readonly != args.readonly {
        // --readonly can only turn it on for this run; the config default wins.
    }
    let readonly = cfg.readonly || args.readonly;

    // ---- archive or delete one session, then stop ----
    //
    // Before the provider is resolved on purpose: filing a conversation away and
    // throwing one out are file operations, and neither may depend on a key, a
    // reachable endpoint, or a parseable provider block. Keeping the list usable is
    // exactly the job you want to be able to do when the network is what is broken.
    match (args.archive.as_deref(), args.delete.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(anyhow!("use either --archive or --delete, not both"));
        }
        (Some(target), None) => {
            let path = resolve_session(target)?;
            let moved = session::archive(&path)?;
            println!("archived {}", moved.display());
            return Ok(0);
        }
        (None, Some(target)) => {
            let path = resolve_session(target)?;
            session::delete(&path)?;
            println!("deleted {}", path.display());
            return Ok(0);
        }
        (None, None) => {}
    }

    // ---- resolve provider ----
    let provider_cfg = cfg.active_provider(args.provider.as_deref())?.clone();
    let mut provider_cfg = provider_cfg;
    if let Some(model) = &args.model {
        provider_cfg.model = model.clone();
    }
    if provider_cfg.model.trim().is_empty() {
        return Err(anyhow!(
            "no model configured for provider '{}'. Set `model` in {}",
            provider_cfg.name,
            config::config_path().display()
        ));
    }
    let key = provider_cfg.resolved_key();
    // A missing key is NOT fatal here. The whole point of the in-tool config is
    // that a fresh machine can start `flint`, run /provider add, and become
    // usable -- refusing to start would lock the user out of the very command
    // that fixes the problem. The check happens when a message is actually sent.
    let key_missing = key.trim().is_empty() && !is_local_endpoint(&provider_cfg.base_url);

    // ---- resume a session, if asked ----
    let mut history: Vec<event::Message> = Vec::new();
    let mut resumed: Option<PathBuf> = None;
    // Kept whole, not just its messages: the transcript is printed from it after the
    // terminal exists, and an empty screen cannot be told apart from a failed load.
    let mut resumed_history: Option<session::LoadedSession> = None;
    if args.continue_last || args.resume.is_some() {
        let target = match &args.resume {
            Some(t) => Some(resolve_session(t)?),
            None => session::latest(&config::sessions_dir())?,
        };
        match target {
            Some(path) => {
                let loaded = session::load(&path)?;
                history = loaded.messages.clone();
                if !loaded.model.is_empty() && args.model.is_none() {
                    provider_cfg.model = loaded.model.clone();
                }
                eprintln!(
                    "flint: resumed {} ({} messages){}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    history.len(),
                    match loaded.title.as_deref() {
                        Some(name) => format!(" — {name}"),
                        None => String::new(),
                    }
                );
                resumed = Some(path);
                resumed_history = Some(loaded);
            }
            None => eprintln!("flint: no previous session found; starting a new one."),
        }
    }

    // ---- debug: answer a question about this run without making one ----
    //
    // Dispatched here, before the session writer and before the terminal, for two reasons
    // that are both about honesty. A diagnostic must not create a session file: a run that
    // sent nothing would leave a conversation behind, and it would be listed by
    // `/sessions` as though something had happened. And its output is stdout, plainly --
    // there is no status row to keep clear if the terminal was never started.
    if let Some(debug) = &args.debug {
        return run_debug(debug, &cfg, &provider_cfg, readonly, &cwd, history);
    }

    // Built before the terminal only because the writer needs the model name; a failure
    // is *deferred*, not fatal.
    //
    // A rescue tool must open its own history when the network is down -- that history is
    // how you find out what you were doing when you broke it. Dying during startup
    // because a proxy is closed takes away the one thing that still works.
    let mut provider_error: Option<String> = None;
    let provider = match provider::Provider::new(provider_cfg.clone()) {
        Ok(p) => p,
        Err(e) => {
            provider_error = Some(format!("{e:#}"));
            provider::Provider::new(provider::Provider::fallback_config())?
        }
    };
    // Continuing a conversation appends to the same file, so nothing said after
    // `--continue` is lost.
    let writer = match &resumed {
        Some(path) => Some(session::SessionWriter::resume(path)?),
        None => Some(session::SessionWriter::create(
            &config::sessions_dir(),
            &cwd,
            &provider_cfg.name,
            &provider_cfg.model,
        )?),
    };
    // Naming is an appended event, so it works the same whether this run started the
    // conversation or continued it. Done here, before the writer is handed to the agent,
    // so the name is on disk before the first turn is asked for.
    if let Some(name) = args.name.as_deref() {
        if let Some(writer) = &writer {
            writer.title(name)?;
        }
    }

    let mut agent = agent::Agent::new(&cfg, provider, readonly, cwd.clone(), writer);
    if !history.is_empty() {
        agent.splice_loaded_history(&cfg, &cwd, history);
    }

    // ---- a run whose output is a stream of JSON objects ----
    //
    // Dispatched here, before the terminal exists, because in this mode stdout *is* the
    // stream and nothing else may touch it: no status row, no warning line, no trailing
    // `println!()`. Threading a flag through `run_turn` would mean re-deciding every choice
    // it makes -- the clock, the answer strip, the redraw, the interrupt prompt -- and every
    // one of them is wrong for a program reading the output.
    if args.json {
        let prompt = args.prompt.as_deref().ok_or_else(|| {
            anyhow!("--json needs a prompt: `flint -p \"...\" --json`. Try --help.")
        })?;
        return run_json_turn(&mut agent, &provider_cfg, provider_error.as_deref(), prompt).await;
    }

    // The terminal comes first: it decides whether there is an input row to keep
    // clear, and the printer needs it for every line it emits.
    //
    // One-shot mode has no input row to reserve -- the caller wants the text and
    // nothing else -- so it leaves the terminal alone.
    //
    // Except under `FLINT_TERM_CAPTURE`, which is the only way to record the
    // interactive byte stream without a keyboard: the REPL's own input path needs a
    // real console to read, so a one-shot prompt is the only turn a test can drive.
    // Without this the strip, the status row and the transcript machinery are
    // unreachable from a test, which is exactly how they came to be checked only by
    // hand.
    let interactive_mode = args.prompt.is_none() || term::capture_requested();
    let term = std::sync::Arc::new(if interactive_mode {
        term::Term::start()?
    } else {
        term::Term::plain()
    });

    // Lets transcript lines show `src/term.rs` instead of the full path the model wrote.
    term.set_cwd(&cwd.to_string_lossy());

    // The capture harness draws the interactive layout but has no console to read keys
    // from -- crossterm would block on one that is not there -- so its input still comes
    // from the pipe, which is what lets a script drive a whole session.
    let (reader, mut input_rx) = if term.interactive() && !term::capture_requested() {
        InputReader::from_terminal(std::sync::Arc::clone(&term))
    } else {
        InputReader::from_stdin()
    };

    let printer = Printer::new(
        color,
        if cfg.verbose { CHATTY } else { NORMAL },
        &term,
    );
    // Tool output is opt-in, and separate from how much of the model's activity is
    // narrated -- printing a file the model read is not "more verbose", it is a
    // different thing.
    printer.set_tool_detail(cfg.tool_detail);

    // Tools have no terminal handle, and must not write to stderr: with the strip
    // active, stderr lands inside it and wrecks the layout. They get a line of the
    // transcript instead.
    {
        let sink_term = std::sync::Arc::clone(&term);
        tools::set_notice_sink(Box::new(move |message| sink_term.notice(message)));
    }

    // A running command's progress goes to the status row, not the transcript: one line
    // per percent of a download would bury the conversation under its own transport. The
    // count of what it is doing is what the status row already exists to show.
    {
        let sink_term = std::sync::Arc::clone(&term);
        tools::set_progress_sink(Box::new(move |line| {
            let line = line.trim();
            // Blank lines are the common case during a transfer, and they say nothing.
            if !line.is_empty() {
                sink_term.activity_detail(line);
            }
        }));
    }

    // ---- the browser window, if it was asked for ----
    //
    // Started after the terminal exists, so the URL lands in the transcript where a person
    // can select it, and before any turn runs, so a message typed into the browser arrives
    // at a conversation that is already there. A failure to bind is fatal rather than a
    // warning: `--web` was the whole point of the run, and a run that quietly served
    // nothing while looking like it worked is the failure this project keeps designing
    // against.
    // Created whether or not `--web` is on, so the turn's plumbing does not change shape
    // between the two: a `None` feed is one branch in `run_turn` and nothing else.
    let live = if args.web { Some(web::Live::new()) } else { None };
    if args.web {
        let window = web::Window::open(
            args.port.unwrap_or(0),
            agent.session_path(),
            live.clone(),
        )
        .await?;
        printer.term().line(format_args!(
            "{} {}",
            printer.dim("web:"),
            window.url()
        ));
    }

    // Search is offered only when it can actually work, so when it cannot the reason has to
    // be said once. Otherwise a `[search]` block that is missing a key is indistinguishable
    // from a flint that has no search at all, and the model simply never has the tool.
    if let search::Availability::Unavailable(why) = search::resolve(&cfg) {
        printer.term().line(format_args!("{} {why}", printer.dim("search:")));
    }

    // A provider that could not be configured is reported now that something can be
    // read, rather than having taken the whole process down before the terminal existed.
    if let Some(error) = &provider_error {
        printer.term().line(format_args!("{} {error}", printer.style(RED, "warning:")));
        printer.term().line(format_args!(
            "{}",
            printer.dim("  flint still works for reading history and running commands")
        ));
    }

    // Show what was resumed. Loading a conversation and showing an empty screen is
    // indistinguishable from loading nothing, and the transcript is the reason for
    // resuming at all.
    if let Some(loaded) = &resumed_history {
        print_transcript(&loaded.messages, &printer);
    }

    // ---- one-shot ----
    if let Some(prompt) = args.prompt {
        run_turn(
            &mut agent,
            &provider_cfg,
            &prompt,
            &printer,
            &mut input_rx,
            false,
            live.as_deref(),
        )
        .await?;
        println!();
        return Ok(0);
    }

    // ---- interactive ----
    let result = interactive(
        &mut cfg,
        &mut agent,
        &mut provider_cfg,
        &printer,
        key_missing,
        &reader,
        &mut input_rx,
        live.as_deref(),
    )
    .await;
    term.stop();
    result?;
    Ok(0)
}

/// Input, delivered over a channel so a turn can run while the keyboard stays
/// live.
///
/// A real terminal is read with crossterm on its own thread, because a turn has
/// to be interruptible by typing and crossterm's reader blocks. Without a
/// terminal -- piped input, a script -- plain lines are read instead, which is
/// also what keeps `flint < file` and shell pipelines working.
struct InputReader {
    /// Present only when there is a terminal to answer questions on.
    req_tx: Option<tokio::sync::mpsc::UnboundedSender<InputReq>>,
}

#[derive(Debug)]
enum InputMsg {
    /// A submitted line.
    Line(String),
    /// Stop the turn in flight, but keep the process: Ctrl-C once.
    Interrupt,
    /// The user asked to quit (Ctrl-D, EOF, or Ctrl-C twice).
    Quit,
}

/// A request sent *to* the key thread.
enum InputReq {
    /// Show this prompt and deliver the next line to the given channel.
    Ask(String, tokio::sync::oneshot::Sender<String>),
}

impl InputReader {
    /// A terminal: key events, edited into lines here in the reader thread.
    fn from_terminal(
        term: std::sync::Arc<term::Term>,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<InputMsg>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<InputMsg>();
        let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<InputReq>();
        std::thread::Builder::new()
            .name("flint-keys".to_string())
            .spawn(move || {
                let mut pending: Option<tokio::sync::oneshot::Sender<String>> = None;
                loop {
                    // Requests first: a wizard may be waiting on an answer.
                    while let Ok(req) = req_rx.try_recv() {
                        match req {
                            InputReq::Ask(prompt, reply) => {
                                term.set_prefix(&prompt);
                                pending = Some(reply);
                                term.redraw();
                            }
                        }
                    }

                    let Ok(ev) = crossterm::event::read() else {
                        let _ = tx.send(InputMsg::Quit);
                        return;
                    };
                    match term.on_event(ev) {
                        term::Key::Enter(line) => {
                            term.set_prefix("> ");
                            if let Some(reply) = pending.take() {
                                // A wizard question: answer it here rather than
                                // handing the line to the REPL.
                                let _ = reply.send(line);
                                term.redraw();
                            } else if tx.send(InputMsg::Line(line)).is_err() {
                                return;
                            }
                        }
                        term::Key::Quit => {
                            if let Some(reply) = pending.take() {
                                let _ = reply.send(String::new());
                            }
                            let _ = tx.send(InputMsg::Quit);
                            return;
                        }
                        term::Key::Interrupt => {
                            // Stop the turn, keep the process. Nothing is running when the
                            // line is empty and no turn is in flight, so this is also the
                            // "did that do anything?" case -- say so rather than exiting.
                            if let Some(reply) = pending.take() {
                                let _ = reply.send(String::new());
                            }
                            let _ = tx.send(InputMsg::Interrupt);
                        }
                        term::Key::Redraw => term.redraw(),
                        term::Key::Ignore => {}
                    }
                }
            })
            .ok();
        (
            InputReader {
                req_tx: Some(req_tx),
            },
            rx,
        )
    }

    /// Ask a question on the input row and wait for the answer.
    ///
    /// Returns `None` when input ends before an answer arrives.
    async fn ask(&self, prompt: String) -> Option<String> {
        let tx = self.req_tx.as_ref()?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send(InputReq::Ask(prompt, reply_tx)).ok()?;
        reply_rx.await.ok()
    }

    /// No terminal: read lines directly.
    fn from_stdin() -> (Self, tokio::sync::mpsc::UnboundedReceiver<InputMsg>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<InputMsg>();
        std::thread::Builder::new()
            .name("flint-stdin".to_string())
            .spawn(move || {
                use std::io::BufRead;
                let stdin = std::io::stdin();
                for line in stdin.lock().lines() {
                    let Ok(line) = line else { break };
                    if tx.send(InputMsg::Line(line)).is_err() {
                        break;
                    }
                }
                let _ = tx.send(InputMsg::Quit);
            })
            .ok();
        (InputReader { req_tx: None }, rx)
    }
}

/// Wait for the next submitted line.
///
/// Polling rather than `recv().await` because callers hold `&InputReader` (to ask
/// wizard questions) while reading lines. A 2 ms tick is far below perception and
/// costs nothing next to a network round trip.
async fn next_line(rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputMsg>) -> Option<InputMsg> {
    loop {
        match rx.try_recv() {
            Ok(msg) => return Some(msg),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        }
    }
}

/// The REPL.
// Eight arguments, and the lint is not wrong -- they are the REPL's collaborators and a
// struct holding them would exist only to satisfy it. They are all `&mut` borrows of state
// that lives in `real_main` for the life of the process, so the struct would be a second
// place that knows how they fit together, for no reader's benefit.
#[allow(clippy::too_many_arguments)]
async fn interactive(
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &mut config::ProviderConfig,
    printer: &Printer<'_>,
    key_missing: bool,
    reader: &InputReader,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputMsg>,
    live: Option<&web::Live>,
) -> Result<()> {

    // Colour codes as this terminal should show them: the names below shadow the
    // bare constants, so {dim} in a format string follows the colour setting
    // instead of leaking an escape code into a pipeline.
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = printer.pal;
    // Two lines. Everything here is something the user may need to check before
    // they trust what follows, and nothing else is worth a line on start-up.
    printer.term().line(format_args!(
        "{bold}flint{reset} {dim}v{}{reset}  {bold}{}{reset}{dim}/{}{reset}  {dim}{}{reset}",
        env!("CARGO_PKG_VERSION"),
        provider_cfg.name,
        provider_cfg.model,
        agent.cwd().display()
    ));
    if agent.readonly() {
        printer.term().line(format_args!(
            "  {}",
            printer.style(GREEN, "readonly — writes and mutating commands are refused")
        ));
    } else if key_missing {
        // Say what to do, and say it where the user is already looking. This is
        // the first thing a fresh machine sees, so it has to be actionable
        // without leaving the tool.
        printer.term().line(format_args!(
            "  {yellow}no API key for this provider{reset}{dim} — shell tools still work. \
             Set one with {reset}{bold}/provider key <key>{reset}{dim}, or add a provider with \
             {reset}{bold}/provider add{reset}"
        ));
    } else {
        printer.term().line(format_args!("  {dim}/help for commands · type while it works to interrupt it{reset}"));
    }
    printer.term().blank();

    loop {
        // The prompt lives on the terminal's reserved row, so there is nothing to
        // print here: the key thread redraws it after every keystroke.
        printer.term().prompt();

        let Some(msg) = next_line(input_rx).await else {
            // The reader thread ends at EOF (Ctrl-D, or piped input done).
            break;
        };
        let input = match msg {
            InputMsg::Quit => break,
            // Nothing is running once the prompt is back, so there is nothing to stop
            // and no reason to take the process with it.
            InputMsg::Interrupt => continue,
            InputMsg::Line(line) => line.trim().to_string(),
        };
        if input.is_empty() {
            continue;
        }

        if let Some(rest) = input.strip_prefix('!') {
            run_shell_escape(cfg, rest, agent.cwd(), printer.term(), printer.pal).await;
            continue;
        }

        if input.starts_with('/') {
            match handle_command(&input, cfg, agent, provider_cfg, printer, reader).await? {
                Flow::Continue => continue,
                Flow::Exit => break,
                Flow::NewAgent(new_agent, new_provider) => {
                    *agent = new_agent;
                    *provider_cfg = new_provider;
                    continue;
                }
            }
        }

        // Echo the message into the transcript. The input row is cleared as soon
        // as Enter is pressed, so without this the conversation above shows only
        // the answers and the questions scroll away unread.
        printer.term().line(format_args!(
            "{bold}> {reset}{}",
            printer.style(BOLD, &input)
        ));

        match run_turn(agent, provider_cfg, &input, printer, input_rx, true, live).await {
            Ok(()) => {}
            Err(e) => {
                printer.term().blank();
                printer.term().line(format_args!("{} {e:#}", printer.style(RED, "error:")));
            }
        }
    }
    Ok(())
}

/// Result of handling a slash command.
///
/// The `NewAgent` variant is large, but replacing an agent happens only when the
/// user switches provider or starts a session, so the size is irrelevant.
#[allow(clippy::large_enum_variant)]
enum Flow {
    Continue,
    Exit,
    /// The provider changed, so both the agent and the provider configuration it was
    /// built from have to replace the ones the REPL is holding.
    ///
    /// Carrying only the agent left `provider_cfg` describing the *previous* provider.
    /// Requests went to the new one -- the agent had it -- while `/model`, `/provider`
    /// and the session header kept reading the old one, so switching to a local provider
    /// and back reported a model that was no longer in use.
    NewAgent(agent::Agent, config::ProviderConfig),
}

/// Ask a question and wait for a line.
///
/// The answer comes from the same channel as everything else the user types.
/// Reading stdin directly would race the key thread -- and on a real terminal it
/// would echo into the middle of the model's output, which is the problem the
/// input row exists to solve.
async fn prompt(
    reader: &InputReader,
    pal: Palette,
    label: &str,
    current: Option<&str>,
) -> Result<String> {
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = pal;
    let shown = match current {
        Some(c) if !c.is_empty() => format!(" {dim}[{c}]{reset}"),
        _ => String::new(),
    };
    let default = current.unwrap_or("").to_string();
    match reader.ask(format!("  {label}{shown}: ")).await {
        Some(v) if v.trim().is_empty() => Ok(default),
        Some(v) => Ok(v.trim().to_string()),
        // Input ended: keep the current value rather than aborting mid-wizard.
        None => Ok(default),
    }
}

/// Ask a yes/no question. An empty answer takes `default`.
async fn confirm(reader: &InputReader, label: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    let answer = reader.ask(format!("  {label} [{hint}]: ")).await;
    Ok(match answer {
        None => default,
        Some(a) => match a.trim().to_ascii_lowercase().as_str() {
            "" => default,
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default,
        },
    })
}

/// Interactive provider setup.
///
/// The point of this function is that the user should never have to leave the
/// tool to make the tool work. A rescue agent whose first instruction is "now
/// go edit a TOML file" has already failed the only scenario it exists for.
async fn provider_wizard(
    cfg: &mut config::Config,
    agent: &agent::Agent,
    editing: Option<&str>,
    printer: &Printer<'_>,
    reader: &InputReader,
) -> Result<Flow> {

    // Colour codes as this terminal should show them: the names below shadow the
    // bare constants, so {dim} in a format string follows the colour setting
    // instead of leaking an escape code into a pipeline.
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = printer.pal;
    let existing = editing.and_then(|n| cfg.provider(n)).cloned();
    let verb = if existing.is_some() {
        "editing"
    } else {
        "new provider"
    };
    printer.term().line(format_args!("\n{bold}{verb}{reset} {dim}(blank = keep current value){reset}"));

    let name = prompt(reader, printer.pal, "name", existing.as_ref().map(|p| p.name.as_str())).await?;
    if name.trim().is_empty() {
        return Err(anyhow!("a provider needs a name"));
    }

    let base_url = prompt(
        reader,
        printer.pal,
        "base_url",
        existing
            .as_ref()
            .map(|p| p.base_url.as_str())
            .or(Some("https://api.deepseek.com/v1")),
    )
    .await?;
    let model = prompt(
        reader,
        printer.pal,
        "model",
        existing
            .as_ref()
            .map(|p| p.model.as_str())
            .or(Some("deepseek-chat")),
    )
    .await?;

    let is_local = is_local_endpoint(&base_url);
    let current_key = existing.as_ref().map(|p| p.api_key.as_str()).unwrap_or("");
    let current_env = existing
        .as_ref()
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_default();

    // Show whether a key is already present without printing it.
    let key_hint = if current_key.trim().is_empty() {
        String::new()
    } else {
        format!("<set, {} chars>", current_key.len())
    };
    printer.term().line(format_args!("{dim}  (the key is stored in plain text in the config file; you can also use an env var instead){reset}"));
    let api_key = prompt(
        reader,
        printer.pal,
        "api_key (or leave blank)",
        if key_hint.is_empty() {
            None
        } else {
            Some(key_hint.as_str())
        },
    )
    .await?;
    // If the user accepted the hint, keep the existing key rather than writing
    // the literal "<set, N chars>" into the config.
    let api_key = if api_key == key_hint {
        current_key.to_string()
    } else {
        api_key
    };

    let env_name = prompt(
        reader,
        printer.pal,
        "api_key_env (read key from this env var; wins over api_key)",
        if current_env.is_empty() {
            None
        } else {
            Some(current_env.as_str())
        },
    )
    .await?;

    let p = config::ProviderConfig {
        name: name.clone(),
        base_url,
        api_key,
        // Editing a provider keeps whatever model list it already had; the wizard asks
        // for the active model, and `/model` is where the rest are chosen.
        models: existing.as_ref().map(|e| e.models.clone()).unwrap_or_default(),
        model,
        api_key_env: if env_name.trim().is_empty() {
            None
        } else {
            Some(env_name)
        },
        // Inherited from the config's shell proxy, which is what a user setting up a
        // provider behind one has already told us.
        proxy: cfg.proxy.clone(),
    };

    let had_key = !p.resolved_key().trim().is_empty();
    let added = cfg.upsert_provider(p);
    cfg.save()?;
    printer.term().line(format_args!(
        "{} {bold}{name}{reset} {} {}",
        printer.style(GREEN, "saved"),
        if added { "added to" } else { "updated in" },
        config::config_path().display()
    ));
    if !had_key && !is_local {
        printer.term().line(format_args!(
            "{}",
            printer.style(
                YELLOW,
                "  no key yet — set one with /provider key, or the request will fail"
            )
        ));
    }

    if confirm(reader, &format!("switch to '{name}' now?"), true).await? {
        cfg.default_provider = name.clone();
        cfg.save()?;
        return switch_provider(cfg, &name, agent.cwd(), agent.readonly(), printer.term(), printer.pal);
    }
    Ok(Flow::Continue)
}

/// Build an agent for `name` and hand it back to the REPL.
///
/// `cwd` and `readonly` are carried over from the running agent rather than
/// re-derived, so switching provider does not silently change where commands
/// run or drop the read-only guard.
fn switch_provider(
    cfg: &config::Config,
    name: &str,
    cwd: &std::path::Path,
    readonly: bool,
    term: &Term,
    pal: Palette,
) -> Result<Flow> {
    // Colour codes as this terminal should show them: the names below shadow the
    // bare constants, so {dim} in a format string follows the colour setting
    // instead of leaking an escape code into a pipeline.
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = pal;
    let target = cfg
        .provider(name)
        .ok_or_else(|| anyhow!("unknown provider '{name}'"))?
        .clone();
    let provider = provider::Provider::new(target.clone())?;
    let writer = Some(session::SessionWriter::create(
        &config::sessions_dir(),
        cwd,
        &target.name,
        &target.model,
    )?);
    let new_agent = agent::Agent::new(cfg, provider, readonly, cwd.to_path_buf(), writer);
    term.line(format_args!(
        "switched to {bold}{}{reset} ({})",
        target.name, target.model
    ));
    Ok(Flow::NewAgent(new_agent, target))
}

async fn handle_command(
    input: &str,
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &mut config::ProviderConfig,
    printer: &Printer<'_>,
    reader: &InputReader,
) -> Result<Flow> {

    // Colour codes as this terminal should show them: the names below shadow the
    // bare constants, so {dim} in a format string follows the colour setting
    // instead of leaking an escape code into a pipeline.
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = printer.pal;
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "/exit" | "/quit" | "/q" => return Ok(Flow::Exit),

        "/help" | "/?" => {
            printer.term().line(format_args!(
                "\
{dim}commands{reset}
  /help                 this message
  /exit                 quit
  /provider             list providers
  /provider <name>      switch to one
  /provider add         set up a new provider (interactive)
  /provider edit <name> change one (interactive)
  /provider key <key>   set the API key for the active provider
  /provider rm <name>   delete one
  /config [edit]        show or change shell, steps, proxy
  /model [name]         show or change the model
  /usage                context and token accounting
  /verbose [on|off|full] how much of the agent's activity to narrate
  /detail [on|off]      print tool output (off: one line per result)
  /readonly [on|off]    toggle the write guard
  /tools                list available tools
  /skills [name]        list skills, or print one as the model would see it
  /sessions             list past sessions, numbered
  /resume <n|id>        switch to one of them
  /name [text]          name this conversation
  /archive <n|id>       file one away, out of the list
  /delete <n|id>        delete one
  /new                  start a fresh conversation
  /reload               re-read the config file (after editing it yourself)
  !<command>            run a shell command without the model
{dim}while the model is working{reset}
  Type and press Enter to interrupt it. Your line becomes the next input.
{dim}notes{reset}
  Permission model is full by default. /readonly is the only guard.
  Everything above is configurable from inside flint; the file is only there
  so that it stays hand-editable when that is easier.
  Config file: {reset}{}",
                config::config_path().display()
            ));
        }

        "/provider" => {
            // Subcommands come first: a provider literally called "add" is a lot
            // less likely than someone wanting to add one.
            let mut sub = arg.splitn(2, char::is_whitespace);
            let first = sub.next().unwrap_or("");
            let rest = sub.next().unwrap_or("").trim();

            match first {
                "" => {
                    printer.term().line(format_args!("{dim}providers:{reset}"));
                    for p in &cfg.providers {
                        let mark = if p.name == provider_cfg.name {
                            "*"
                        } else {
                            " "
                        };
                        let key_state = if p.resolved_key().trim().is_empty()
                            && !is_local_endpoint(&p.base_url)
                        {
                            printer.style(RED, " (no key)")
                        } else {
                            String::new()
                        };
                        printer.term().line(format_args!(
                            "  {mark} {:<12} {:<34} {}{key_state}",
                            p.name, p.base_url, p.model
                        ));
                    }
                    printer.term().line(format_args!(
                        "{dim}  /provider <name>       switch\n  \
                         /provider add          configure a new one\n  \
                         /provider edit <name>  change one\n  \
                         /provider key <key>    set the key for the active one\n  \
                         /provider rm <name>    delete one{reset}"
                    ));
                }

                "add" => return provider_wizard(cfg, agent, None, printer, reader).await,

                "edit" => {
                    if rest.is_empty() {
                        return Err(anyhow!("usage: /provider edit <name>"));
                    }
                    if cfg.provider(rest).is_none() {
                        return Err(anyhow!("unknown provider '{rest}'"));
                    }
                    return provider_wizard(cfg, agent, Some(rest), printer, reader).await;
                }

                "rm" | "remove" | "delete" => {
                    if rest.is_empty() {
                        return Err(anyhow!("usage: /provider rm <name>"));
                    }
                    if cfg.providers.len() <= 1 {
                        return Err(anyhow!(
                            "refusing to delete the last provider — there would be nothing left to talk to"
                        ));
                    }
                    if !cfg.remove_provider(rest) {
                        return Err(anyhow!("unknown provider '{rest}'"));
                    }
                    let fallback = cfg
                        .providers
                        .first()
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    if cfg.default_provider == rest {
                        cfg.default_provider = fallback.clone();
                    }
                    cfg.save()?;
                    printer.term().line(format_args!(
                        "{} removed {bold}{rest}{reset}; default is now {bold}{fallback}{reset}",
                        printer.style(GREEN, "ok")
                    ));
                    if provider_cfg.name == rest {
                        return switch_provider(cfg, &fallback, agent.cwd(), agent.readonly(), printer.term(), printer.pal);
                    }
                }

                "key" => {
                    // A convenience for the common case; /provider edit works too.
                    if rest.is_empty() {
                        return Err(anyhow!("usage: /provider key <api-key>"));
                    }
                    let mut target = provider_cfg.clone();
                    target.api_key = rest.to_string();
                    // An env var would silently win over the key we just set.
                    target.api_key_env = None;
                    cfg.upsert_provider(target.clone());
                    cfg.save()?;
                    printer.term().line(format_args!(
                        "{} key saved for {bold}{}{reset} ({})",
                        printer.style(GREEN, "ok"),
                        target.name,
                        config::config_path().display()
                    ));
                    return switch_provider(cfg, &target.name, agent.cwd(), agent.readonly(), printer.term(), printer.pal);
                }

                _ => {
                    let target = cfg
                        .provider(first)
                        .ok_or_else(|| {
                            anyhow!("unknown provider '{first}' (try /provider add or /provider)")
                        })?
                        .clone();
                    let provider = provider::Provider::new(target.clone())?;
                    let writer = Some(session::SessionWriter::create(
                        &config::sessions_dir(),
                        agent.cwd(),
                        &target.name,
                        &target.model,
                    )?);
                    let new_agent = agent::Agent::new(
                        cfg,
                        provider,
                        agent.readonly(),
                        agent.cwd().clone(),
                        writer,
                    );
                    cfg.default_provider = target.name.clone();
                    cfg.save()?;
                    printer.term().line(format_args!(
                        "switched to {bold}{}{reset} ({})",
                        target.name, target.model
                    ));
                    return Ok(Flow::NewAgent(new_agent, target));
                }
            }
        }

        "/model" => {
            // The same shape as `/provider`: no argument lists what can be chosen and how,
            // an argument chooses. A list that shows the options is the only way the user
            // can know what names are valid -- `/provider` already worked that way, and
            // `/model` disagreeing with it was the reason it read as broken.
            let choices = provider_cfg.choices();
            if arg.is_empty() {
                printer.term().line(format_args!(
                    "{dim}models for provider {}{reset} {dim}({}):{reset}",
                    provider_cfg.name, provider_cfg.base_url
                ));
                for m in &choices {
                    let mark = if *m == provider_cfg.model { "*" } else { " " };
                    printer.term().line(format_args!("  {mark} {m}"));
                }
                printer.term().line(format_args!(
                    "{dim}  /model <name>         switch to it, and save it\n  \
                     add more with `models = [...]` in {}, or /provider edit {}{reset}",
                    config::config_path().display(),
                    provider_cfg.name
                ));
            } else if arg == provider_cfg.model {
                printer.term().line(format_args!(
                    "{dim}already using {}{reset}",
                    provider_cfg.model
                ));
            } else if !choices.iter().any(|m| m == arg) {
                // Refused rather than accepted: a typo would otherwise be sent to the
                // provider as a model name, and the reply would be a server error that
                // says nothing about the real mistake.
                printer.term().line(format_args!(
                    "{} unknown model {bold}{}{reset} for {}. {dim}/model lists them.{reset}",
                    printer.style(RED, "error:"),
                    arg,
                    provider_cfg.name
                ));
            } else {
                // Actually switch. This used to print "set `model` in <config> to make
                // this permanent" and change nothing, which read as a refusal: the
                // command named the setting and then declined to set it.
                let mut target = provider_cfg.clone();
                target.model = arg.to_string();
                let provider = provider::Provider::new(target.clone())?;
                let writer = Some(session::SessionWriter::create(
                    &config::sessions_dir(),
                    agent.cwd(),
                    &target.name,
                    &target.model,
                )?);
                let new_agent = agent::Agent::new(
                    cfg,
                    provider,
                    agent.readonly(),
                    agent.cwd().clone(),
                    writer,
                );
                // Persisted as well as applied: the next run should use it, which is
                // what the old hint was promising the user they had to do by hand.
                cfg.upsert_provider(target.clone());
                cfg.save()?;
                printer.term().line(format_args!(
                    "{} model {bold}{}{reset} for {bold}{}{reset} {dim}(saved){reset}",
                    printer.style(GREEN, "ok"),
                    target.model,
                    target.name
                ));
                return Ok(Flow::NewAgent(new_agent, target));
            }
        }

        "/usage" => match agent.last_usage() {
            Some(u) => {
                printer.term().line(format_args!(
                    "last request:  prompt {bold}{}{reset}  completion {bold}{}{reset}  total {bold}{}{reset} tokens",
                    u.prompt_tokens,
                    u.completion_tokens,
                    u.total()
                ));
                printer.term().line(format_args!(
                    "{dim}prompt tokens = your current context size. Nothing is trimmed automatically; \
                     use /new if it grows too large.{reset}"
                ));
            }
            None => printer.term().line(format_args!("{dim}no usage reported yet by this provider{reset}")),
        },

        "/readonly" => {
            let turn_on = match arg {
                "on" => true,
                "off" => false,
                "" => !agent.readonly(),
                other => return Err(anyhow!("expected on|off, got '{other}'")),
            };
            if turn_on {
                printer.term().line(format_args!(
                    "{}",
                    printer.style(GREEN, "readonly ON — no writes, no mutating commands")
                ));
            } else {
                printer.term().line(format_args!("{}", printer.style(RED, "readonly OFF — full permissions")));
            }
            printer.term().line(format_args!(
                "{dim}note: takes effect on the next /new or restart (the tool set is per agent).{reset}"
            ));
        }

        "/verbose" => {
            let next = match arg {
                "on" | "normal" => NORMAL,
                "off" | "quiet" => QUIET,
                "full" | "all" => CHATTY,
                "" => {
                    if printer.verbosity() == NORMAL {
                        CHATTY
                    } else {
                        NORMAL
                    }
                }
                other => return Err(anyhow!("expected on|off|full, got '{other}'")),
            };
            printer.set_verbosity(next);
            cfg.verbose = next >= CHATTY;
            cfg.save()?;
            let what = match next {
                QUIET => "off — only the model's answers",
                NORMAL => "on — one line per tool call",
                _ => "full — arguments and the reasoning marker",
            };
            printer.term().line(format_args!("{} {what}", printer.style(GREEN, "verbose")));
            if cfg.tool_detail {
                printer.term().line(format_args!(
                    "{dim}note: tool output is still printed in full; /detail off to stop that.{reset}",
                ));
            }
        }

        "/detail" => {
            // Deliberately separate from `/verbose`. Turning up the narration of
            // what the model is doing should not also print every file it reads.
            let on = match arg {
                "on" | "full" => true,
                "off" | "quiet" => false,
                "" => !printer.tool_detail(),
                other => return Err(anyhow!("expected on|off, got '{other}'")),
            };
            printer.set_tool_detail(on);
            cfg.tool_detail = on;
            cfg.save()?;
            let what = if on {
                "on — tool output is printed, up to 25 lines per result"
            } else {
                "off — one line per tool result"
            };
            printer.term().line(format_args!("{} {what}", printer.style(GREEN, "detail")));
        }

        "/config" => {
            printer.term().line(format_args!("{dim}config: {}{reset}", config::config_path().display()));
            printer.term().line(format_args!("  default_provider = {bold}{}{reset}", cfg.default_provider));
            printer.term().line(format_args!(
                "  shell            = {bold}{}{reset} {:?}",
                cfg.shell, cfg.shell_args
            ));
            printer.term().line(format_args!("  max_steps        = {}", cfg.max_steps));
            printer.term().line(format_args!(
                "  proxy            = {}",
                cfg.proxy.as_deref().unwrap_or("(none)")
            ));
            printer.term().line(format_args!("  verbose          = {}", cfg.verbose));
            printer.term().line(format_args!("  tool_detail      = {}", cfg.tool_detail));
            let workspace = context::Workspace::discover(agent.cwd(), &cfg.skill_dirs);
            printer.term().line(format_args!(
                "  instructions     = {}{}",
                cfg.instructions,
                if workspace.instruction_files.is_empty() {
                    format!(
                        " {dim}(no AGENTS.md between {} and the project root){reset}",
                        agent.cwd().display()
                    )
                } else {
                    format!(
                        " {dim}({}){reset}",
                        workspace
                            .instruction_files
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ));
            if !workspace.skills.is_empty() {
                printer.term().line(format_args!(
                    "  skills           = {} {dim}(.flint/skills or ~/.flint/skills){reset}",
                    workspace.skills.len()
                ));
            }
            // The value in force, not the value in the file.
            //
            // These differ whenever `--readonly` or `/readonly` is used, and reporting
            // the file's value then describes a permission the session does not have: a
            // real session was left hunting the config for a setting that was not the
            // one in effect.
            if agent.readonly() == cfg.readonly {
                printer.term().line(format_args!("  readonly         = {}", cfg.readonly));
            } else {
                printer.term().line(format_args!(
                    "  readonly         = {bold}{}{reset} {dim}(this run; the file says {}){reset}",
                    agent.readonly(),
                    cfg.readonly
                ));
            }

            if arg == "edit" {
                // A small wizard, so the settings that matter when you are
                // stuck are reachable without hand-editing TOML.
                printer.term().line(format_args!("\n{bold}settings{reset} {dim}(blank = keep){reset}"));
                let shell = prompt(reader, printer.pal, "shell", Some(&cfg.shell)).await?;
                let args = prompt(
                    reader,
                    printer.pal,
                    "shell_args (space separated)",
                    Some(&cfg.shell_args.join(" ")),
                )
                .await?;
                let steps = prompt(reader, printer.pal, "max_steps", Some(&cfg.max_steps.to_string())).await?;
                let proxy = prompt(
                    reader,
                    printer.pal,
                    "proxy (e.g. http://127.0.0.1:10808, blank to clear)",
                    cfg.proxy.as_deref(),
                )
                .await?;

                cfg.shell = shell;
                cfg.shell_args = args.split_whitespace().map(str::to_string).collect();
                cfg.max_steps = steps.parse().unwrap_or(cfg.max_steps);
                cfg.proxy = if proxy.trim().is_empty() {
                    None
                } else {
                    Some(proxy)
                };
                cfg.save()?;
                printer.term().line(format_args!(
                    "{} saved to {}",
                    printer.style(GREEN, "ok"),
                    config::config_path().display()
                ));
            } else {
                printer.term().line(format_args!("{dim}  /config edit   change shell, steps, proxy{reset}"));
                printer.term().line(format_args!("{dim}  (provider settings: /provider){reset}"));
            }
        }

        "/tools" => {
            printer.term().line(format_args!("{dim}tools:{reset} {}", agent.tool_names().join(", ")));
        }

        "/skills" => {
            // Deliberately re-discovered here rather than remembered, and deliberately
            // showing what the model would actually be given: `/skills <name>` prints the
            // same body the `skill` tool returns, so "did it load what I wrote" is
            // answerable without asking the model.
            let workspace = context::Workspace::discover(agent.cwd(), &cfg.skill_dirs);
            if arg.is_empty() {
                if workspace.skills.is_empty() {
                    printer.term().line(format_args!("{dim}no skills found{reset}"));
                    printer.term().line(format_args!(
                        "{dim}  a skill is <dir>/<name>/SKILL.md, with `name` and \
                         `description` in front matter{reset}"
                    ));
                } else {
                    printer.term().line(format_args!("{dim}skills:{reset}"));
                    for skill in &workspace.skills {
                        printer.term().line(format_args!(
                            "  {:<20} {dim}{}{reset}",
                            skill.name,
                            skill.path.display()
                        ));
                        printer.term().line(format_args!("      {}", skill.description));
                    }
                }
                let searched: Vec<String> = workspace
                    .skill_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect();
                printer
                    .term()
                    .line(format_args!("{dim}  searched: {}{reset}", searched.join(", ")));
            } else {
                for line in workspace.load(arg)?.lines() {
                    printer.term().line(format_args!("{line}"));
                }
            }
        }

        "/sessions" => {
            let sessions = session::list(&config::sessions_dir())?;
            if sessions.is_empty() {
                printer.term().line(format_args!("(no sessions yet)"));
            }
            // Numbered so the number can be typed straight back: `/resume 3`. The id is
            // still shown, because that is the name of the file and the thing to quote.
            for (index, (id, summary)) in sessions.iter().enumerate() {
                printer.term().line(format_args!("  {:>2}. {id}  {summary}", index + 1));
            }
            if !sessions.is_empty() {
                printer.term().line(format_args!(
                    "{}",
                    printer.dim("     /resume <number|id> to continue one of these")
                ));
            }
        }

        "/resume" => {
            // Switch conversations without restarting.
            //
            // Restarting is what `--resume` needs, but this is a rescue tool: the process
            // may be the only thing still working on the machine, and losing it to change
            // which conversation is on screen would be a poor trade. `/new` already
            // rebuilds the agent, so this only changes which file it appends to.
            if arg.is_empty() {
                printer
                    .term()
                    .line(format_args!("usage: /resume <number|id>  (/sessions to list)"));
                return Ok(Flow::Continue);
            }
            let path = resolve_session(arg)?;
            let loaded = session::load(&path)?;
            let count = loaded.messages.len();
            if !loaded.model.is_empty() {
                provider_cfg.model = loaded.model.clone();
            }
            let provider = provider::Provider::new(provider_cfg.clone())?;
            let writer = Some(session::SessionWriter::resume(&path)?);
            let cwd = agent.cwd().clone();
            let mut new_agent = agent::Agent::new(
                cfg,
                provider,
                agent.readonly(),
                cwd.clone(),
                writer,
            );
            // Behind a *freshly built* system prompt, never the loaded messages as they
            // stand. A session file holds the conversation and not the prompt -- the prompt
            // is rebuilt at startup because it carries run-time facts, and a file written
            // from another directory or by another build may carry one that is no longer
            // true. Assigning the loaded messages directly dropped the fresh prompt
            // entirely when the file had none, which left the model with no instructions at
            // all and no sign that anything was wrong: the REPL looked normal, and the
            // answers just got worse.
            new_agent.splice_loaded_history(cfg, &cwd, loaded.messages);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            printer.term().line(format_args!(
                "{green}resumed:{reset} {name} ({count} messages){}",
                if loaded.model.is_empty() {
                    String::new()
                } else {
                    format!(", model {}", provider_cfg.model)
                }
            ));
            return Ok(Flow::NewAgent(new_agent, provider_cfg.clone()));
        }

        "/reload" => {
            // Re-read the config from disk and rebuild the agent.
            //
            // Necessary because the agent can edit its own config with the file
            // tools: without this, a change it just made would not take effect
            // until the process restarted, which is exactly the "leave the tool
            // to fix the tool" problem this is meant to avoid.
            let fresh = config::Config::load()?;
            let target = fresh.active_provider(None)?.clone();
            *cfg = fresh;
            let provider = provider::Provider::new(target.clone())?;
            *provider_cfg = target.clone();
            let writer = Some(session::SessionWriter::create(
                &config::sessions_dir(),
                agent.cwd(),
                &provider_cfg.name,
                &provider_cfg.model,
            )?);
            let new_agent =
                agent::Agent::new(cfg, provider, agent.readonly(), agent.cwd().clone(), writer);
            let state = if target.resolved_key().trim().is_empty()
                && !is_local_endpoint(&target.base_url)
            {
                printer.style(YELLOW, " (still no key)")
            } else {
                String::new()
            };
            printer.term().line(format_args!(
                "{} reloaded {} — provider {bold}{}{reset} model {bold}{}{reset}{state}",
                printer.style(GREEN, "ok"),
                config::config_path().display(),
                target.name,
                target.model
            ));
            return Ok(Flow::NewAgent(new_agent, target));
        }

        "/name" => {
            // A name is one appended line, so this behaves the same on a conversation
            // that was just started and one that was resumed -- neither needs a rewrite,
            // and the newest name is the one in force.
            let Some(path) = agent.session_path() else {
                printer
                    .term()
                    .line(format_args!("{dim}this conversation is not being saved{reset}"));
                return Ok(Flow::Continue);
            };
            if arg.is_empty() {
                let title = session::scan(&path)?.title;
                printer.term().line(format_args!(
                    "{dim}name:{reset} {}",
                    title.unwrap_or_else(|| "(unnamed)".to_string())
                ));
                return Ok(Flow::Continue);
            }
            agent.name_session(arg)?;
            printer.term().line(format_args!("{green}named:{reset} {arg}"));
        }

        "/archive" | "/delete" => {
            // Both take an explicit session and both refuse the one that is open.
            //
            // Refusing is not a limitation, it is the only honest answer: this process is
            // appending to that file, so deleting it would leave the writer pointing at a
            // path that no longer exists (and the next event would recreate the file),
            // while moving it would hide the conversation being written. `/new` first,
            // then file the old one away by its number.
            if arg.is_empty() {
                printer
                    .term()
                    .line(format_args!("usage: {cmd} <number|id>  (/sessions to list)"));
                return Ok(Flow::Continue);
            }
            let path = resolve_session(arg)?;
            if agent.session_path().as_deref() == Some(path.as_path()) {
                printer.term().line(format_args!(
                    "{dim}that is the conversation you are in. /new starts a fresh one; \
                     then this one can be filed away by its number.{reset}"
                ));
                return Ok(Flow::Continue);
            }
            if cmd == "/archive" {
                let moved = session::archive(&path)?;
                printer.term().line(format_args!("archived {}", moved.display()));
            } else {
                session::delete(&path)?;
                printer.term().line(format_args!("deleted {}", path.display()));
            }
        }

        "/new" => {
            let provider = provider::Provider::new(provider_cfg.clone())?;
            let writer = Some(session::SessionWriter::create(
                &config::sessions_dir(),
                agent.cwd(),
                &provider_cfg.name,
                &provider_cfg.model,
            )?);
            let new_agent =
                agent::Agent::new(cfg, provider, agent.readonly(), agent.cwd().clone(), writer);
            printer.term().line(format_args!("started a new session"));
            return Ok(Flow::NewAgent(new_agent, provider_cfg.clone()));
        }

        other => {
            printer.term().line(format_args!("{dim}unknown command '{other}'. /help for the list.{reset}"));
        }
    }
    Ok(Flow::Continue)
}

/// Execute one user turn, streaming output to the terminal.
/// Refuse to send a message when the active provider has no key.
///
/// This is checked at send time rather than at start-up on purpose: flint must
/// still *open* without a key, because the command that sets one lives inside
/// it.
fn ensure_usable(provider_cfg: &config::ProviderConfig) -> Result<()> {
    if provider_cfg.resolved_key().trim().is_empty() && !is_local_endpoint(&provider_cfg.base_url) {
        let env_hint = provider_cfg
            .api_key_env
            .as_deref()
            .unwrap_or("YOUR_API_KEY_ENV");
        return Err(anyhow!(
            "provider '{}' has no API key.\n\
             Fix it without leaving flint:\n  \
             /provider key <your-key>     save a key for this provider\n  \
             /provider add                configure a different provider\n\
             Or set the environment variable {env_hint}, or edit {}.",
            provider_cfg.name,
            config::config_path().display()
        ));
    }
    Ok(())
}

/// One turn, written as NDJSON on stdout.
///
/// The session file is written exactly as in any other run -- the stream is a view of the
/// turn, not a replacement for the record -- so a `--json` run can be listed, resumed and
/// read afterwards like anything else.
///
/// The one thing this mode does not carry over is the interrupt handling in `run_turn`:
/// there is no keyboard to press and no strip to prompt on. Ctrl-C ends the process, and
/// the session file keeps every event that was complete when it did.
async fn run_json_turn(
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    provider_error: Option<&str>,
    prompt: &str,
) -> Result<i32> {
    let mut out = std::io::stdout();
    // Flushed per line: a consumer may be reading the stream as it arrives, and a
    // block-buffered pipe would deliver the whole run at the end, which is the one thing a
    // stream must not do.
    let mut emit = |line: String| {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };

    emit(ndjson::session_started(
        agent.session_path().as_deref(),
        agent.cwd(),
        &provider_cfg.model,
    ));
    // A provider that could not be configured is a warning, not a failure: the run may
    // still work, and saying so on the stream is how a caller learns why nothing came back.
    if let Some(error) = provider_error {
        emit(ndjson::warning(error));
    }
    emit(ndjson::turn_started(prompt));

    // The checks come after those three lines, not before, so that a run which cannot even
    // start is still described on stdout. Failing out to `main`'s error path would put the
    // reason on stderr and leave the stream empty, which is indistinguishable from a run
    // that is still thinking -- and a caller reading the stream would wait for a
    // `turn.completed` that is never coming.
    if let Err(e) = ensure_usable(provider_cfg) {
        emit(ndjson::error(&format!("{e:#}")));
        return Ok(1);
    }

    let mut sink = ndjson::Sink::new();
    let result = agent
        .run(prompt, |event| {
            if let Some(line) = sink.line(&event) {
                emit(line);
            }
        })
        .await;

    match result {
        Ok(()) => {
            emit(ndjson::turn_completed(agent.last_usage()));
            Ok(0)
        }
        // The failure goes on the stream as well as the exit code: a caller that reads
        // stdout should not have to also read stderr to find out what happened.
        Err(e) => {
            emit(ndjson::error(&format!("{e:#}")));
            Ok(1)
        }
    }
}

/// What the turn is waiting for, said once so that both renderers say the same thing.
///
/// The terminal has a status row and the browser has a status line, and they are the same
/// fact. `run_turn` decides it here and nowhere else. `Term` owns the clock and is the only
/// thing that knows whether a call *started* a wait or merely renamed one -- a repeated tool
/// name keeps its original start time on purpose -- so the answer is taken from it rather
/// than guessed by the caller. A browser that guessed would reset its clock thirty seconds
/// into a wait and show `0s` where the terminal shows `30s`.
fn status_waiting(printer: &Printer<'_>, live: Option<&web::Live>, text: &str) {
    let restarted = printer.term().activity_started(text);
    announce_status(printer, live, restarted);
}

/// The same wait, named differently. The clock keeps counting.
fn status_named(printer: &Printer<'_>, live: Option<&web::Live>, text: &str) {
    // Nothing running: naming it would invent an activity with no start time, and the
    // browser must not be told about a phase the terminal is not showing.
    if !printer.term().activity_named(text) {
        return;
    }
    announce_status(printer, live, false);
}

/// Nothing is being waited for any more.
fn status_done(printer: &Printer<'_>, live: Option<&web::Live>) {
    printer.term().activity_done();
    announce_status(printer, live, false);
}

/// What the status row now says, to whoever else is rendering this run.
///
/// The words come from `Term::activity_label` and not from the caller's argument. They are
/// not the same thing: an unnamed wait is shown as `waiting for the model`, and a stream
/// carrying the raw empty name would blank the browser's status line at the exact moment
/// the wait began -- which is the whole thing this event exists to prevent.
fn announce_status(printer: &Printer<'_>, live: Option<&web::Live>, restarted: bool) {
    if let Some(live) = live {
        live.event(&Event::Status {
            text: printer.term().activity_label(),
            restarted,
        });
    }
}

async fn run_turn(
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    input: &str,
    printer: &Printer<'_>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputMsg>,
    // Whether a line arriving mid-turn is a person interrupting. It is not when the
    // lines come from a pipe: `echo OK | flint -p "..."` would have its own input read
    // as steering, and the answer would be cancelled before it started -- which is
    // exactly what `flint -p ... | cat` used to do.
    can_steer: bool,
    // The browser's feed, when `--web` is running. Everything the turn produces is copied
    // here as well as drawn on the terminal, so the two cannot describe different runs.
    live: Option<&web::Live>,
) -> Result<()> {
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = printer.pal;
    ensure_usable(provider_cfg)?;

    let mut current = input.to_string();

    // Say something before the request goes out, not only after a tool starts.
    //
    // Streamed output is buffered once per attempt so that a retry can discard it and
    // run again without duplicating text on screen, which means nothing appears until
    // the whole response has arrived. Without a status line that leaves the terminal
    // completely still for the length of the model's response -- and a model that is
    // slow, or an endpoint that is dead, looks exactly like a program that has hung.
    // The empty name is what makes the line read "waiting for the model" rather than
    // naming a tool.
    // Every model call starts here, including the ones after a tool round. The status
    // line carries the phase from here on; the escalation in the wait loop turns the
    // nameless wait into an explicit "no response yet".
    status_waiting(printer, live, "");

    loop {
        let mut tool_names: HashMap<String, (String, String)> = HashMap::new();
        // The calls of the current round, in the order they were announced, so the clock
        // can move to the next one as each result arrives.
        let mut round_calls: Vec<String> = Vec::new();
        let mut streamed_text = false;
        // The answer so far. Streamed output is redrawn in full on every fragment,
        // because a fragment is not a line: it can stop in the middle of a word,
        // and only the whole text can be placed correctly.
        let mut answer = String::new();
        // The accumulator above belongs to this turn, and so does the terminal's record
        // of what has already been committed: starting the next turn has to start that
        // record over too, or the answer is measured against the previous turn's text.
        printer.term().begin_answer();

        // A tool round is another wait: the step starts by asking the model again, and
        // the clock has to be running for it or the pause after every tool call looks
        // like the turn is over.
        status_waiting(printer, live, "");

        // The turn and its output closure are confined to this scope: the future
        // holds a mutable borrow of `buffer`, and that borrow has to end before
        // the code below can read `buffer` to decide what to flush.
        let (result, steering) = {
            let mut turn = Box::pin(agent.run(&current, |event| {
                // A phase change is announced *before* the thing that caused it, so a reader
                // of the stream sees the two in the order the terminal does: `writing the
                // answer`, then the text. Emitting the event first and the status from
                // inside its arm put them the wrong way round, which the first live capture
                // showed immediately. `activity_named` is idempotent, so repeating this for
                // every fragment of one answer costs nothing.
                match &event {
                    Event::Text(_) if !streamed_text => status_named(printer, live, WRITING_LABEL),
                    // The same two guards the arm below used to carry, kept together with
                    // the announcement they belong to: a fragment arriving after the answer
                    // has started, or a blank one, is not a phase.
                    Event::Reasoning(t) if !streamed_text && !t.trim().is_empty() => {
                        status_named(printer, live, THINKING_LABEL)
                    }
                    _ => {}
                }
                // Every event the turn produces, on the browser's stream as well. This
                // closure is the one place they all pass, which is what makes the browser a
                // renderer of the same run rather than a reconstruction of it.
                if let Some(live) = live {
                    live.event(&event);
                }
                match event {
                Event::Text(t) => {
                    if printer.term().interactive() {
                        // The answer is redrawn in full, so the terminal always
                        // shows a complete line even when a fragment stops
                        // mid-word.
                        answer.push_str(&t);
                        printer.term().stream(&answer);
                    } else {
                        // A pipeline gets each fragment once. Re-rendering here
                        // would print the whole answer again per fragment.
                        printer.term().stream(&t);
                    }
                    streamed_text = true;
                }
                Event::Reasoning(_) => {
                    // The model thinking out loud.
                    //
                    // Not put in the transcript: the provider emits one event per SSE
                    // fragment, so there is no natural place to break the block into
                    // lines, and the whole reasoning spread down the screen is what a
                    // live turn used to look like. It is not thrown away either -- it is
                    // in the session file.
                    //
                    // What the user needs from it is *that it is happening*, and that
                    // belongs on the status line, which is already showing how long the
                    // turn has taken. A separate `… thinking` marker in the transcript
                    // said the same thing a second time, one row above, and left the
                    // status line claiming the model was still being waited for while it
                    // was in fact already talking. The label is put up by the match above,
                    // before this event reaches the stream.
                }
                Event::ToolStart { id, name } => {
                    // Start the clock as soon as the tool is known, not when its
                    // arguments have finished streaming: the wait begins here, and a
                    // tool that never returns is exactly the case this is for.
                    //
                    // Only for the first call of the round, though. Every call in a round
                    // is announced before any of them runs, so naming the clock on each
                    // announcement claimed the *last* call was the one being waited for,
                    // and reset the elapsed time of a tool that had not started yet.
                    if round_calls.is_empty() {
                        status_waiting(printer, live, &name);
                    }
                    round_calls.push(id.clone());
                    tool_names.insert(id, (name, String::new()));
                }
                Event::ToolArgs { id, args } => {
                    let name = tool_names
                        .get(&id)
                        .map(|(name, _)| name.clone())
                        .unwrap_or_default();
                    printer.tool_call(&name, &args);
                    // Remembered so the result line can name what it was done to. Without
                    // it, two reads of two different files both print "read N lines" and
                    // the transcript looks like a duplicate.
                    if let Some(entry) = tool_names.get_mut(&id) {
                        entry.1 = args;
                    }
                }
                Event::ToolResult { id, output, ok } => {
                    // The name comes from the matching ToolStart: the result is
                    // reported as "鉁?read" or "鉁?bash", so the transcript says what
                    // happened rather than just that something did.
                    let (name, args) = tool_names
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| (String::new(), String::new()));
                    printer.tool_result(&name, &args, &output, ok);
                    tool_names.remove(&id);
                    round_calls.retain(|call| call != &id);
                    // The clock belongs to whatever is being waited for now: the next call
                    // in the round, or -- once they have all run -- the model that has to
                    // be asked for the round after this one. Clearing it here instead left
                    // the rest of a multi-call round and the whole following model call
                    // with no clock at all, which is the pause a user actually stares at.
                    let next = round_calls
                        .first()
                        .and_then(|next| tool_names.get(next))
                        .map(|(name, _)| name.clone())
                        .unwrap_or_default();
                    status_waiting(printer, live, &next);
                }
                Event::Usage(_) => {}
                Event::Warning(w) => {
                    printer.term().blank();
                    printer.term().line(format_args!("{} {w}", printer.style(RED, "warning:")));
                }
                Event::Done => {}
                // The terminal never receives one of these: the status *is* the terminal's
                // own row, and `status_waiting` reads from it rather than feeding it. A
                // caller that could send one would be a second place deciding the phase.
                Event::Status { .. } => {}
                }
            }));

            // Whichever happens first: the model finishes this turn, or the user
            // says something. Dropping the future cancels the HTTP stream, which
            // is exactly what an interrupt should do.
            //
            // Polling rather than a channel select, for the same reason as
            // next_line: the stream is woken by the network, the input by the tick.
            let mut result = None;
            let mut steering = None;
            let mut interrupted = false;
            loop {
                // Only a submitted line interrupts. `Quit` here means stdin ended
                // (a one-shot run, or a script that closed the pipe) -- treating
                // it as steering would abort the turn before it ever started,
                // which is exactly what `flint -p ...  | cat` used to do.
                match if can_steer {
                    input_rx.try_recv()
                } else {
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                } {
                    Ok(InputMsg::Line(line)) => {
                        steering = Some(line);
                        break;
                    }
                    // A bare Ctrl-C stops the turn but keeps the process: the session is
                    // the record of what was being fixed, and losing it to a reflex is the
                    // expensive mistake. It has to be said out loud, or a cancelled turn
                    // is indistinguishable from one that finished with nothing to say.
                    Ok(InputMsg::Interrupt) => {
                        interrupted = true;
                        break;
                    }
                    Ok(InputMsg::Quit) | Err(_) => {}
                }
                tokio::select! {
                    r = &mut turn => {
                        result = Some(r);
                        break;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(2)) => {
                        // Keep the running-status line moving. This loop is already
                        // waiting, so the clock costs one comparison per tick and only
                        // repaints when its number changes.
                        // Distinguish a slow model from one that is not answering at
                        // all. Until something arrives there is no way to tell them
                        // apart, so the label escalates once the silence is long enough
                        // to mean something -- which is the case the user is actually
                        // staring at when they wonder whether it has hung.
                        if printer.term().activity_is_unnamed_wait()
                            && printer.term().activity_elapsed() >= NO_RESPONSE_AFTER
                        {
                            status_named(printer, live, NO_RESPONSE_LABEL);
                        }
                        printer.term().tick();
                    }
                }
            }
            if interrupted {
                printer
                    .term()
                    .notice("stopped -- the model is not running any more");
            }
            (result, steering)
        };

        // Whatever happened, nothing is running now: leaving a stale clock on the strip
        // would be worse than showing none.
        status_done(printer, live);

        match steering {
            None => {
                // The turn finished. `stream` already rendered the whole answer, so
                // there is no trailing fragment left to flush -- only the line
                // break that ends it.
                if streamed_text {
                    printer.term().end_stream();
                }
                if let Some(r) = result {
                    r?;
                }
                break;
            }
            Some(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                if streamed_text {
                    // A partial answer is on screen and is no longer valid.
                    printer.term().blank();
                }
                printer.interrupted();
                printer.term().line(format_args!("{bold}> {reset}{}", printer.dim(&text)));
                current = text;
            }
        }
    }

    if let Some(u) = agent.last_usage() {
        printer.term().line(format_args!(
            "{}",
            printer.style(
                DIM,
                &format!(
                    "[ctx {} prompt + {} completion = {} tokens]",
                    u.prompt_tokens,
                    u.completion_tokens,
                    u.total()
                )
            )
        ));
    }
    Ok(())
}

/// `!cmd` escape inside the REPL and the `exec` subcommand.
async fn run_shell_escape(
    cfg: &config::Config,
    command: &str,
    cwd: &std::path::Path,
    term: &Term,
    pal: Palette,
) {
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = pal;
    if command.trim().is_empty() {
        term.line(format_args!("usage: !<command>"));
        return;
    }
    match tools::run_command_raw(cfg, command, cwd, 600).await {
        Ok(out) => term.text_ln(&out),
        Err(e) => term.line(format_args!("{red}error:{reset} {e:#}")),
    }
}

async fn exec_direct(
    cfg: &config::Config,
    command: &str,
    cwd: &std::path::Path,
    term: &Term,
) -> Result<i32> {
    // A direct exec is meant for real work (installs, rebuilds), so it gets a
    // generous ceiling rather than the conversational default.
    let outcome = tools::run_command_detailed(cfg, command, cwd, 1800).await?;
    term.text_ln(&outcome.report);
    // Propagate the child's status. `flint exec` is meant to be usable from
    // scripts, so a failing command must make flint itself fail -- reporting
    // success here would make the exit code meaningless.
    Ok(outcome.code)
}

fn is_local_endpoint(base_url: &str) -> bool {
    provider::is_local_endpoint(base_url)
}

/// `flint debug <what>`: print something about this run without making one.
///
/// Nothing here contacts the provider, writes a session, or creates a terminal. What it
/// prints comes from the same code the real path uses, which is the only way the answer
/// stays true: a diagnostic that re-implements the thing it describes reports on the
/// re-implementation, and is believed.
fn run_debug(
    words: &[String],
    cfg: &config::Config,
    provider_cfg: &config::ProviderConfig,
    readonly: bool,
    cwd: &std::path::Path,
    history: Vec<event::Message>,
) -> Result<i32> {
    match words[0].as_str() {
        "prompt-input" => {
            // No session writer, so nothing is created on disk for a run that sends
            // nothing. The provider is built for its model name and its body builder, not
            // to talk to anything -- an unreachable endpoint is fine here, and a missing
            // API key is not an error.
            let provider = provider::Provider::new(provider_cfg.clone())?;
            let mut agent = agent::Agent::new(cfg, provider, readonly, cwd.to_path_buf(), None);
            if !history.is_empty() {
                agent.splice_loaded_history(cfg, cwd, history);
            }
            // Everything after the subcommand is the message that would be sent, so this
            // answers "what would the model read if I said this" as well as "what does it
            // read now".
            let said = words[1..].join(" ");
            let next = if said.trim().is_empty() {
                None
            } else {
                Some(said.as_str())
            };
            // Pretty-printed, unlike the one-object-per-line `--json` stream: whitespace
            // between JSON tokens carries no meaning, and the whole point of this command
            // is that a person can read the answer.
            println!("{}", serde_json::to_string_pretty(&agent.request_preview(next))?);
            Ok(0)
        }
        other => Err(anyhow!(
            "unknown debug command '{other}'. Known: prompt-input [message]"
        )),
    }
}

/// Status-line labels, one per thing the turn can be waiting for.
///
/// "Waiting" is not one state. The request going out, the model reasoning, the model
/// emitting an answer, and a tool running are four different waits with four different
/// meanings, and a single label told the user none of them.
///
/// The initial wait has no label: an unnamed activity already reads "waiting for the
/// model", and a constant for it would be a second way to say the same thing.
/// How long silence may last before it stops looking like a model thinking.
const NO_RESPONSE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
const NO_RESPONSE_LABEL: &str = "no response yet — the network or the endpoint may be stuck";
const THINKING_LABEL: &str = "thinking";
const WRITING_LABEL: &str = "writing the answer";

/// Reduce a tool's JSON arguments to the one value worth showing on a line.
fn parse_args(argv: Vec<String>) -> Result<Args> {
    let mut args = Args::default();
    let mut iter = argv.into_iter().peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => args.help = true,
            "-p" | "--prompt" => {
                args.prompt = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--prompt requires a value"))?,
                )
            }
            "-c" | "--continue" => args.continue_last = true,
            // `--resume` with a value is a specific session; bare, it means the most
            // recent one, which is what it used to mean and what `-c` already does.
            "--resume" => {
                let next = iter.peek().cloned().unwrap_or_default();
                if next.is_empty() || next.starts_with('-') || next == "exec" {
                    args.continue_last = true;
                } else {
                    iter.next();
                    args.resume = Some(next);
                }
            }
            "--provider" => {
                args.provider = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--provider requires a value"))?,
                )
            }
            "--model" => {
                args.model = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--model requires a value"))?,
                )
            }
            "--readonly" | "--no-edit" => args.readonly = true,
            "--no-color" => args.no_color = true,
            "--json" => args.json = true,
            "--web" => args.web = true,
            "--port" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("--port requires a number (0 means any free port)"))?;
                args.port = Some(
                    value
                        .parse()
                        .map_err(|_| anyhow!("--port needs a number from 0 to 65535, not '{value}'"))?,
                );
            }
            "--list-sessions" => args.list_sessions = true,
            "--name" => {
                args.name = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--name requires a value"))?,
                )
            }
            "--archive" => {
                args.archive = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--archive requires a session"))?,
                )
            }
            "--delete" => {
                args.delete = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--delete requires a session"))?,
                )
            }
            "--cwd" => {
                args.cwd = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--cwd requires a value"))?,
                )
            }
            "exec" => {
                let rest: Vec<String> = iter.by_ref().collect();
                if rest.is_empty() {
                    return Err(anyhow!("exec requires a command"));
                }
                args.exec = Some(rest.join(" "));
            }
            "debug" => {
                let rest: Vec<String> = iter.by_ref().collect();
                if rest.is_empty() {
                    return Err(anyhow!(
                        "debug requires a subcommand. Known: prompt-input [message]"
                    ));
                }
                args.debug = Some(rest);
            }
            other if other.starts_with('-') => {
                return Err(anyhow!("unknown flag '{other}'. Try --help."))
            }
            other => {
                // Bare words are treated as a one-shot prompt, which makes
                // `flint why is my dsh broken` work.
                args.prompt = Some(match args.prompt.take() {
                    Some(existing) => format!("{existing} {other}"),
                    None => other.to_string(),
                });
            }
        }
    }

    // An argument-consistency check, so it belongs with the arguments: a person who typed
    // `--port 8080` meant to be served something. Ignoring the flag would leave them with a
    // run that looks like it worked and a browser that cannot connect.
    if args.port.is_some() && !args.web {
        return Err(anyhow!(
            "--port needs --web: it chooses the port the browser view listens on"
        ));
    }
    // `--json` writes one turn to stdout and exits, so a window opened beside it would close
    // before anything could be read from it. Saying so beats starting a listener that is gone
    // by the time the browser connects.
    if args.json && args.web {
        return Err(anyhow!(
            "--web and --json cannot both own the run: --json exits after one turn, taking \
             the window with it. Use --web alone, or --json alone."
        ));
    }
    Ok(args)
}

fn print_help(color: bool, term: &Term) {
    // The escape sequences are applied only when colour is wanted, so
    // `flint --no-color --help` is plain text (and piping stays clean).
    let (b, r) = if color { (BOLD, RESET) } else { ("", "") };
    term.line(format_args!(
        "\
{b}flint{r} — a minimal cross-platform command-line agent

{b}USAGE{r}
  flint                            interactive session
  flint -p \"<prompt>\"              one-shot, prints the answer and exits
  flint -p \"<prompt>\" --json       the same run as one JSON object per line
  flint <words...>                 same as -p
  flint --continue                 resume the most recent session
  flint --resume <n|id>            resume a particular session
  flint exec <command>             run a command directly (no model, no network)
  flint debug prompt-input [msg]   print the request that would be sent, and send nothing
  flint --list-sessions            list saved sessions, numbered for --resume
  flint --name <text>              name this conversation (also: /name)
  flint --archive <n|id>           file a session away, out of the list
  flint --delete <n|id>            delete a session file

{b}OPTIONS{r}
  --provider <name>   use a specific provider          (config: default_provider)
  --model <name>      override the model for this run
  --readonly          refuse writes and mutating commands
  --json              with -p: write the run as NDJSON on stdout
  --web               also serve a browser view of this run on 127.0.0.1
  --port <n>          the port for --web (default 0: any free one)
  --cwd <dir>         working directory for tools
  --no-color          disable ANSI colour (also honours NO_COLOR)
  -h, --help          this message

{b}CONFIG{r}
  {}

{b}WHY{r}
  A command-line agent that works directly on your machine: reading and writing
  code, running commands, searching a tree, setting a machine up, debugging what
  is broken, or just answering a question. It is deliberately small,
  dependency-light and hand-editable — which is also why it is still there when
  your usual tooling is not.",
        config::config_path().display()
    ));
}
