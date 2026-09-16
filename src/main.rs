//! flint — a minimal cross-platform command-line agent.
//!
//! Modes:
//!   flint                          interactive REPL
//!   flint -p "..."                 one-shot prompt
//!   flint --continue               resume the most recent session in this directory
//!   flint exec "npm i -g ..."      run a command directly, no model involved
//!   flint --list-sessions          show past sessions
//!
//! The `exec` mode is the last line of defence: when every provider is
//! unreachable, flint still runs commands.

use flint::{
    agent, config, context, display, engine, event, ndjson, provider, schema, search, session, sink,
    term, tools, web,
};

use anyhow::{anyhow, Context, Result};
use display::{Printer, BOLD, CHATTY, DIM, GREEN, NORMAL, QUIET, RED, RESET, YELLOW};
#[allow(unused_imports)]
use display::Palette;
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
    /// A session to copy and continue instead of one to continue in place: the same three ways of
    /// naming it as `--resume`, and empty for the most recent.
    ///
    /// `cp` already does this, which is why it is a flag and not a file format: the point is that
    /// "take this conversation and branch it" should not require knowing where flint keeps its
    /// sessions, or that a conversation is one file.
    fork: Option<String>,
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
    /// Ask the provider whether it can be used, and what is left in the account, then stop.
    ///
    /// A preflight: the same `insufficient_balance` a failed run reports, asked before a batch instead
    /// of after it. Cheap by construction -- it never sends a completion -- and `--json` gives it a
    /// shape a program can read.
    balance: bool,
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
    /// A JSON Schema the answer must satisfy: a file path, or the schema itself when it starts with `{`.
    ///
    /// Kept as the text that was given, not as a parsed schema, because the run has to be able to
    /// report a schema it could not use: `--schema broken.json` should say which keyword it choked
    /// on rather than exit with an argument-parsing complaint. Parsing happens where the answer
    /// shape becomes a fact about the run -- see `resolve_schema`.
    schema: Option<String>,
    /// Answer in prose even if this session's file says it was last held to a schema.
    ///
    /// The counterpart to a `schema` line following the session: a container of a resumed
    /// conversation has to be able to say "not this time" without editing the file.
    no_schema: bool,
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

/// What the exit code means, so a caller can branch without reading text.
///
/// A program that runs flint looks at the code first, and until now every failure was `1`: a typo in
/// an argument, a missing key, an unreachable endpoint and an answer that never matched its schema
/// were the same signal. The convention here is `sysexits.h`, which exists for exactly this reason --
/// *"a CLI that always exits 0 (or always 1) hides this signal, forcing agents to parse error text
/// with regex"*. The numbers are the ones a reader will already know:
const EXIT_OK: i32 = 0;
/// A failure that is not yet classified. The one honest answer while the classification is being
/// built: say "something went wrong" rather than name the wrong cause.
const EXIT_FAILURE: i32 = 1;
/// The command line is wrong: nothing was asked of the model, and changing the arguments fixes it.
/// `2` rather than `sysexits.h`'s 64 because it is the number every `getopt` program already uses.
const EXIT_USAGE: i32 = 2;
/// `EX_DATAERR`: the answer is not usable as it stands -- a schema that never matched, or a turn
/// that stopped asking before the model was finished.
const EXIT_DATAERR: i32 = 65;
/// `EX_UNAVAILABLE`: the provider cannot be used at all. Retrying the same question changes nothing.
const EXIT_UNAVAILABLE: i32 = 69;
/// `EX_TEMPFAIL`: a failure worth retrying, by a caller that can wait.
#[allow(dead_code)]
const EXIT_TEMPFAIL: i32 = 75;
/// `128 + SIGINT`: the run was cut short -- `/stop` on a pipe, or a Ctrl-C on a terminal.
const EXIT_INTERRUPTED: i32 = 130;

/// An error the caller can fix by changing the command line, rather than one flint has to report.
///
/// Marked where the cause is known, not recognised later from the message text: if the exit code is
/// going to be read by a program, the classification has to come from the code that knows what went
/// wrong. This is the smallest form of that, and the shape the finer codes will follow.
#[derive(Debug)]
struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

/// Refuse what the caller asked for, in a way `main` can turn into [`EXIT_USAGE`].
fn usage<T>(message: impl Into<String>) -> Result<T> {
    Err(Usage(message.into()).into())
}

/// Mark every failure inside a block of argument handling as a usage error.
///
/// The whole parse is wrapped rather than each of its forty refusals: every one of them is about the
/// command line by construction, and a refusal added later inherits the right code instead of
/// silently defaulting to "something went wrong".
fn as_usage<T>(result: Result<T>) -> Result<T> {
    result.map_err(|e| Usage(format!("{e:#}")).into())
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
            // Read from the error itself rather than guessed at here: the code is a promise to a
            // program, and a promise made from the outside is the kind that breaks quietly.
            if e.downcast_ref::<Usage>().is_some() {
                EXIT_USAGE
            } else {
                EXIT_FAILURE
            }
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
    // The listing carries each session's *path*, and the resolver uses it. A session is not always
    // directly in `sessions/`: it lives in the subdirectory belonging to the working directory it
    // was held in, so joining an id onto the root names a file that is not there -- which is how
    // `/delete 1` came to report a missing file instead of doing what it was asked.
    let sessions = session::list_detailed(&dir)?;

    if let Ok(index) = target.parse::<usize>() {
        if index == 0 || index > sessions.len() {
            return Err(anyhow!(
                "no session {index}: /sessions lists {} (1 is the most recent)",
                sessions.len()
            ));
        }
        return Ok(sessions[index - 1].path.clone());
    }

    let prefix = target.trim_end_matches(".jsonl");
    // The archive is searched by id as well as the live directories: a conversation that
    // was filed away is still something you may want to read, and `mv` by hand must not
    // make it unreachable. It is not offered by number, because the numbers are the ones
    // `/sessions` prints and that list does not include the archive.
    let mut candidates: Vec<(String, PathBuf)> = sessions
        .into_iter()
        .map(|summary| (summary.id, summary.path))
        .collect();
    candidates.extend(
        session::list_archived(&dir)?
            .into_iter()
            .map(|summary| (summary.id, summary.path)),
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
    let args = as_usage(parse_args(std::env::args().skip(1).collect()))?;

    // Decide colour before anything prints.
    let color = colour_allowed(args.no_color);

    if args.help {
        print_help(color, &Term::plain());
        return Ok(0);
    }

    // The directory this run works in, resolved once and absolutely.
    //
    // Three things have to agree on it: the tools run in it, the session's `meta` line records it,
    // and `--continue` finds a conversation by comparing the two. A relative `--cwd` recorded as
    // written would be resolved later against whatever directory the *next* process started in,
    // and a program driving flint one process per question starts somewhere different every time
    // -- so the conversation would become unfindable by the caller that created it, which is the
    // quietest possible way to lose one.
    //
    // `absolute`, not `canonicalize`: canonicalising a path on Windows prepends the verbatim `\\?\`
    // prefix, and this path is *recorded*, in a file meant to be read and edited by hand. Symlinks
    // are left alone for the same reason -- the session should say where the run was asked to work,
    // not where that turned out to point. Spelling differences between two paths to one directory
    // are handled where they matter, by the comparison in `session::latest_for`.
    //
    // A directory that does not exist is refused rather than created: every tool in the run would
    // fail for a reason that has nothing to do with the question, which reads as flint being
    // broken, and creating it would leave a directory behind from a run that was never meant to
    // happen.
    let cwd = match &args.cwd {
        Some(dir) => {
            let path = std::path::absolute(dir)
                .with_context(|| format!("--cwd {dir}: cannot be resolved"))?;
            if !path.is_dir() {
                return usage(format!(
                    "--cwd {dir}: {}",
                    if path.exists() {
                        "not a directory"
                    } else {
                        "no such directory"
                    }
                ));
            }
            path
        }
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
            return usage("use either --archive or --delete, not both");
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

    // ---- the preflight, before anything is spent or opened ----
    //
    // Here rather than beside `--list-sessions`, because it needs the provider that was just resolved
    // and nothing else: no session, no terminal, no prompt. That is also why it can answer the two
    // questions a batch has -- "is this usable" and "how much is left" -- at a cost of one request.
    if args.balance {
        return balance_and_stop(provider_cfg.clone(), args.json).await;
    }

    // ---- resume a session, if asked ----
    //
    // `--fork` opens a conversation too, so it is refused here rather than silently winning: both
    // flags say which file the run writes, and a program that picks one of two answers to that is
    // a program that can lose an afternoon's work to a shell history line.
    if args.fork.is_some() && (args.continue_last || args.resume.is_some()) {
        return Err(anyhow!(
            "--fork copies a session and --resume/--continue write to the one they open; give one"
        ));
    }
    let mut history: Vec<event::Message> = Vec::new();
    let mut resumed: Option<PathBuf> = None;
    // The conversation `--fork` copied, as the pair the seeding needs: the messages to write into
    // the new file and the name to carry with them. Kept here rather than in `resumed` because the
    // two do opposite things with the file -- a resume appends to the session it opened, and a fork
    // must not touch it at all.
    let mut forked: Option<(Vec<event::Message>, Option<String>)> = None;
    // Kept whole, not just its messages: the transcript is printed from it after the
    // terminal exists, and an empty screen cannot be told apart from a failed load.
    let mut resumed_history: Option<session::LoadedSession> = None;
    if args.continue_last || args.resume.is_some() || args.fork.is_some() {
        let target = match (&args.resume, &args.fork) {
            (Some(t), _) => Some(resolve_session(t)?),
            (_, Some(t)) if !t.is_empty() => Some(resolve_session(t)?),
            _ => session::latest_for(&config::sessions_dir(), &cwd)?,
        };
        match target {
            Some(path) => {
                let loaded = session::load(&path)?;
                history = loaded.messages.clone();
                if !loaded.model.is_empty() && args.model.is_none() {
                    provider_cfg.model = loaded.model.clone();
                }
                let copied = args.fork.is_some();
                eprintln!(
                    "flint: {} {} ({} messages){}",
                    if copied { "forking" } else { "resumed" },
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    history.len(),
                    match loaded.title.as_deref() {
                        Some(name) => format!(" — {name}"),
                        None => String::new(),
                    }
                );
                if copied {
                    forked = Some((history.clone(), loaded.title.clone()));
                } else {
                    resumed = Some(path);
                }
                resumed_history = Some(loaded);
            }
            // Which directory is the whole point of the message. `--continue` means "the
            // conversation I was just in *here*", so what a caller needs to hear is that nothing
            // has happened in this directory -- not that no session exists anywhere, which was
            // the old wording and was said while some other project's conversation sat in the
            // same home. A caller that asked to continue and silently got a fresh conversation
            // has lost the thread it thought it was holding.
            None => eprintln!(
                "flint: no session for {} yet; starting a new one.",
                cwd.display()
            ),
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
    // `--continue` is lost. A fork is the one case where the conversation and the file come
    // apart, and the file has to be *seeded* with what was copied rather than left empty: a fork
    // whose context held the conversation but whose session did not is the `/model` bug again --
    // a transcript on screen that no file contains, and a page tailing a session that starts
    // mid-sentence.
    let writer = match (&resumed, &forked) {
        (Some(path), _) => Some(session::SessionWriter::resume(path)?),
        (None, Some((messages, title))) => Some(session::SessionWriter::seed(
            &config::sessions_dir(),
            &cwd,
            &provider_cfg.name,
            &provider_cfg.model,
            messages,
            title.as_deref(),
        )?),
        (None, None) => Some(session::SessionWriter::create(
            &config::sessions_dir(),
            &cwd,
            &provider_cfg.name,
            &provider_cfg.model,
        )?),
    };
    // A fork writes a file the run has just made, and until it has a name the two sessions are
    // indistinguishable in the only place it matters -- `/sessions`, five minutes later, where the
    // choice is between the original and the branch. The original is named too, because "untouched"
    // is worth saying out loud for the one command that could have been a resume by mistake.
    if forked.is_some() {
        if let Some(writer) = &writer {
            eprintln!(
                "flint: forked into {}; the original is untouched",
                writer
                    .path()
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
        }
    }
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

    // ---- what shape this run's answers must take ----
    //
    // Resolved after the agent exists and before the first turn, because the three inputs are only
    // all known here: the flag, the schema the resumed file records, and whether the caller said
    // "none". `hold_to_schema` puts it in the prompt *and* in the request, and `--no-schema` on a
    // session that carried one is recorded as a decision rather than left as a silence the next
    // reader would have to interpret.
    let shaping = resolve_output_schema(&args, resumed_history.as_ref())?;
    if let Some(schema) = &shaping.schema {
        agent.hold_to_schema(Some(schema.clone()));
    }
    if shaping.stated {
        agent.record_schema(shaping.schema_json.as_ref())?;
    }

    // ---- a run whose output is a stream of JSON objects ----
    //
    // Dispatched here, before the terminal exists, because in this mode stdout *is* the
    // stream and nothing else may touch it: no status row, no warning line, no trailing
    // `println!()`. Threading a flag through `run_turn` would mean re-deciding every choice
    // it makes -- the clock, the answer strip, the redraw, the interrupt prompt -- and every
    // one of them is wrong for a program reading the output.
    if args.json {
        let prompt = args.prompt.as_deref().ok_or_else(|| -> anyhow::Error {
            Usage("--json needs a prompt: `flint -p \"...\" --json`. Try --help.".to_string()).into()
        })?;
        return run_json_turn(
            &mut agent,
            &provider_cfg,
            provider_error.as_deref(),
            prompt,
            &shaping,
        )
        .await;
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

    let printer = Printer::new(color, cfg.verbose.level(), &term);
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

    // ---- the engine, if this provider needs one running ----
    //
    // Here rather than at the first request, because the wait is the honest part: a local
    // model takes tens of seconds to load, and a flint that accepted a prompt and then sat
    // there would look broken. A failure is a warning and not a stop, for the same reason a
    // missing API key is not one: the run may be about to fix the thing that is wrong.
    if let Err(e) = engine::ensure_up(&provider_cfg, &cfg, |line| {
        printer.term().line(format_args!("{}", printer.dim(&line)))
    })
    .await
    {
        printer.term().line(format_args!("{} {e:#}", printer.style(YELLOW, "warning:")));
    }

    // ---- the browser window, if it was asked for ----
    //
    // Started after the terminal exists, so the URL lands in the transcript where a person
    // can select it, and before any turn runs, so a message typed into the browser arrives
    // at a conversation that is already there. A failure to bind is fatal rather than a
    // warning: `--web` was the whole point of the run, and a run that quietly served
    // nothing while looking like it worked is the failure this project keeps designing
    // against.
    //
    // `/web` opens the same thing later, so this is a `Viewer` and not a `Window`: the REPL
    // holds it and can bind it part-way through, and a run started without `--web` differs
    // only in that nobody has asked yet.
    let mut viewer = if args.web {
        Some(web::Viewer::asked(
            args.port.unwrap_or(0),
            agent.session_path(),
            browser_input(&reader, args.prompt.is_none()),
        ))
    } else {
        None
    };
    if let Some(viewer) = viewer.as_mut() {
        let url = viewer.open(0).await?;
        announce_view(&printer, &url);
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
            viewer.as_ref().map(web::Viewer::live),
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
        &mut viewer,
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
    /// The same sender the reader thread writes to.
    ///
    /// Kept so that the browser can be a **second producer** on the prompt rather than a
    /// parallel path into the agent: a line typed into the page arrives here and becomes
    /// `InputMsg::Line`, which is the event a keystroke produces. That is what makes every
    /// slash command work from the browser without the browser knowing what one is, and what
    /// makes a message sent while the model is running steer the turn the way typing does.
    line_tx: tokio::sync::mpsc::UnboundedSender<InputMsg>,
    /// Whether the source of input has finished -- Ctrl-D, or a pipe closing.
    ///
    /// Recorded rather than inferred, and the reason is a bug this caused. `run_turn`
    /// deliberately **consumes and drops** a `Quit` that arrives while a turn is running, since
    /// a pipe closing is not a person asking to stop. So the end of input is a fact that can be
    /// observed exactly once and then lost.
    ///
    /// That was invisible while the sender lived and died with the reader thread: the channel
    /// closed on its own and the REPL left on `Disconnected`. The browser put a second producer
    /// on this channel, which keeps it open for the life of the process -- and then a dropped
    /// `Quit` meant a turn that failed with a closed pipe behind it left flint waiting forever
    /// for input that could not come. Measured with `printf 'a\n/exit\n' | flint` against a dead
    /// endpoint: the error printed and the process never exited.
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug)]
enum InputMsg {
    /// A submitted line.
    Line(String),
    /// A listing the page asked for in its own panel: a command line, run with the terminal quiet.
    ///
    /// Separately from `Line` because the two differ in what happens to the *answer*, and this is
    /// the only place that can be said: a typed line prints here and lands in the transcript, and a
    /// report is handed to the page instead (§8 keeps a listing off this terminal's screen when
    /// nobody here asked for it).
    Report(String),
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
        // Cloned before the thread takes `tx`: this is the handle the browser writes through,
        // and both producers have to be the same channel for a browser line to *be* a keystroke.
        let line_tx = tx.clone();
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended_in_thread = std::sync::Arc::clone(&ended);
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
                        // After the message, never before: the REPL treats "empty and ended"
                        // as the end of input, and a flag set first could let it reach that
                        // conclusion while this `Quit` was still in flight.
                        ended_in_thread.store(true, std::sync::atomic::Ordering::SeqCst);
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
                            ended_in_thread.store(true, std::sync::atomic::Ordering::SeqCst);
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
                line_tx,
                ended,
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
        let line_tx = tx.clone();
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended_in_thread = std::sync::Arc::clone(&ended);
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
                ended_in_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .ok();
        (
            InputReader {
                req_tx: None,
                line_tx,
                ended,
            },
            rx,
        )
    }
}

/// Wait for the next submitted line.
///
/// Polling rather than `recv().await` because callers hold `&InputReader` (to ask
/// wizard questions) while reading lines. A 2 ms tick is far below perception and
/// costs nothing next to a network round trip.
///
/// Empty is not the same as over, and that distinction is the whole of this function. It used
/// to be the same by accident: the sender belonged to the reader thread, so when the input ran
/// out the channel closed and `Disconnected` said so. The browser is now a second producer on
/// this channel, which keeps it open for the life of the process, so "there will never be
/// another line" has to be asked separately -- see `InputReader::ended`.
async fn next_line(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputMsg>,
    reader: &InputReader,
) -> Option<InputMsg> {
    loop {
        match rx.try_recv() {
            Ok(msg) => return Some(msg),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                // Only when the channel is *empty*, so a `Quit` still queued behind a finished
                // reader is drained before this gives up. Losing it would be harmless -- the
                // answer is the same -- but draining keeps the two paths identical.
                if reader.ended.load(std::sync::atomic::Ordering::SeqCst) {
                    return None;
                }
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
    viewer: &mut Option<web::Viewer>,
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

    // What a turn left for the REPL: a line it handed back, and the reports the page asked for while
    // it was working. See `Handover`.
    let mut pending = Handover::default();

    loop {
        // The prompt lives on the terminal's reserved row, so there is nothing to
        // print here: the key thread redraws it after every keystroke.
        printer.term().prompt();

        // Tell the page what its controls would show, if that is not what it was told.
        //
        // Here, at the top of the loop, because this is the one place every command comes back
        // to -- including the ones that replace the agent -- so a command cannot forget to say
        // that it changed something, and the frame cannot drift from the configuration it
        // describes. `Live::state` drops it when it has not changed, so the cost of asking on
        // every line is a comparison.
        if let Some(viewer) = viewer.as_ref() {
            viewer.live().state(state_frame(cfg, provider_cfg, agent, printer));
        }

        // What the page asked to read while the model was working. Flushed here, before the prompt
        // is read again, and outside the `input` path below on purpose: a report is not input, it
        // never reaches the model, and the only reason it is in this loop at all is that a command
        // has to run where the configuration and the agent are.
        for asked in std::mem::take(&mut pending.reports) {
            run_report(&asked, cfg, agent, provider_cfg, printer, reader, viewer).await;
        }

        // A line the turn could not use -- a command typed or clicked while the model was
        // working. It is handled here, next time round the loop, as if it had just been typed:
        // `run_turn` cannot run a command, and this is the only place that knows what a typed
        // line means.
        let input = match pending.line.take() {
            Some(line) => line.trim().to_string(),
            None => {
                let Some(msg) = next_line(input_rx, reader).await else {
                    // The reader thread ends at EOF (Ctrl-D, or piped input done).
                    break;
                };
                match msg {
                    InputMsg::Quit => break,
                    // Nothing is running once the prompt is back, so there is nothing to stop
                    // and no reason to take the process with it.
                    InputMsg::Interrupt => continue,
                    InputMsg::Line(line) => line.trim().to_string(),
                    // A report the page asked for with the prompt sitting idle. Same handling as the
                    // flush above, and `continue` rather than falling through: there is no prompt
                    // here for the model, and an empty string would `continue` anyway.
                    InputMsg::Report(asked) => {
                        run_report(&asked, cfg, agent, provider_cfg, printer, reader, viewer).await;
                        continue;
                    }
                }
            }
        };
        if input.is_empty() {
            continue;
        }

        if let Some(rest) = input.strip_prefix('!') {
            run_shell_escape(cfg, rest, agent.cwd(), printer.term(), printer.pal).await;
            continue;
        }

        // A command-line flag typed at the prompt means the slash command it names, so
        // `--web` opens the view rather than being sent to the model as a sentence. See
        // `flag_at_the_prompt`: an exact flag is translated, a sentence containing one is
        // left alone, and a flag with no equivalent inside a running process says so.
        let input = match flag_at_the_prompt(&input) {
            Some((command, why)) if !command.is_empty() => {
                printer.term().line(format_args!(
                    "{dim}{input} is a start-up flag — doing {command} instead ({why}){reset}"
                ));
                command
            }
            Some((_, why)) => {
                printer
                    .term()
                    .line(format_args!("{yellow}{input}: {why}{reset}"));
                continue;
            }
            None => input,
        };

        if input.starts_with('/') {
            // A command's answer, for the page as well as the terminal.
            //
            // Commands answer through `printer.term()`, which is a terminal and nothing else, so a
            // command typed into the page's composer used to print nothing the page could read.
            // Recording starts here and stops below, which is what keeps a *turn's* lines out of
            // it: they go through the same funnel and are already frames of their own.
            if viewer.is_some() {
                printer.term().answer_start();
            }
            let flow = match handle_command(&input, cfg, agent, provider_cfg, printer, reader, viewer).await {
                Ok(flow) => flow,
                // A command that fails is an answer, not the end of the session.
                //
                // These arms return `Result`, and this used to hand the error straight out of
                // `interactive` with `?` -- where it became the process's exit status. So one
                // mistyped word (`/verbose loud`) printed `flint: error: ...` and killed the
                // conversation. The page made it worse: its composer sends lines to the same
                // place, so a mistyped command in the browser ended the run the page was
                // watching, and the browser could not even see why -- the message went to stderr.
                //
                // The line the input reader refuses is answered two branches above, for the same
                // reason and in the same shape. One `line` call per line of the message: an error
                // carrying a newline would otherwise move the cursor down through the rows the
                // layout reserved.
                Err(why) => {
                    for (n, text) in format!("{why:#}").lines().enumerate() {
                        let said = if n == 0 {
                            format!("{input}: {text}")
                        } else {
                            text.to_string()
                        };
                        printer.term().line(format_args!("{red}{said}{reset}"));
                    }
                    Flow::Continue
                }
            };
            if let Some(viewer) = viewer.as_ref() {
                let said = printer.term().answer_take();
                viewer.live().command(echoed_input(&input), &said);
            }
            match flow {
                Flow::Continue => continue,
                Flow::Exit => break,
                Flow::NewAgent(new_agent, new_provider) => {
                    // The page follows whichever file the run is writing, and it is told here rather
                    // than in each command because `/new` and `/resume` do move the run to another
                    // file. `/model`, `/provider` and `/reload` no longer do -- they replace the
                    // agent around the conversation it is already in -- so this is a no-op for them,
                    // which is exactly what the page wants: it went on tailing the file nobody was
                    // writing when they *did* move, and the fix for that is this call.
                    // The session changes first and the readers are told second, so a page that
                    // re-reads on the `reset` reads the file the run is now writing rather than
                    // the one it just left. A no-op when the file is the same one.
                    if let Some(viewer) = viewer.as_mut() {
                        viewer.follow(new_agent.session_path());
                    }
                    *agent = new_agent;
                    *provider_cfg = new_provider;
                    continue;
                }
            }
        }

        // Echo the message into the transcript. The input row is cleared as soon
        // as Enter is pressed, so without this the conversation above shows only
        // the answers and the questions scroll away unread.
        //
        // One `line` call per line, because a pasted block arrives with its line breaks
        // intact and a single call carrying a newline would move the cursor down through
        // the rows the layout has reserved. The continuation lines are indented under the
        // prompt marker so the block still reads as one message rather than as several.
        for (n, line) in input.lines().enumerate() {
            let marker = if n == 0 { ">" } else { " " };
            printer.term().line(format_args!(
                "{bold}{marker} {reset}{}",
                printer.style(BOLD, line)
            ));
        }

        match run_turn(
            agent,
            provider_cfg,
            &input,
            printer,
            input_rx,
            true,
            viewer.as_ref().map(web::Viewer::live),
        )
        .await {
            // A command that arrived mid-turn, and any reports the page asked for: both are handled
            // at the top of the loop, in that order -- see `Handover`.
            Ok(handover) => pending = handover,
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

/// Ask this machine to show a URL. Returns whether a program was started.
///
/// **Best effort, and it has to be.** The URL is printed either way, so the failure mode here
/// is "you paste it yourself", which is all the program did before this existed. Not waited
/// on: a browser starting up is not something flint should block behind, and its exit code
/// says nothing about whether a page appeared.
///
/// Called only when stdout is a terminal. Opening a window is a courtesy to a person, and a
/// pipe is not a person -- without that check, `cargo test` would launch browsers.
fn open_in_browser(url: &str) -> bool {
    let run = browser_invocation(url);
    let mut cmd = std::process::Command::new(&run.program);
    flint::tools::apply_invocation(&mut cmd, &run);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// The command that shows a URL, as a program and the one argument that must not be re-quoted.
///
/// Windows needs the invocation `cmd` documents for a command *line*: `/S /C`, and the whole
/// line quoted, with the URL inside quotes of its own. Measured here by putting the character
/// under test in the *name* of a `.cmd` file standing in for the browser, so that nothing
/// opened a window -- one quoted argument, in the position the URL sits:
///
/// | target | as a plain argument | `/S /C` and the line verbatim |
/// |---|---|---|
/// | `plain.cmd` | ran | ran |
/// | `a&b.cmd` | did not run: cmd read `&` as a new command | ran |
/// | `a^b.cmd` | did not run: the caret was eaten | ran |
/// | `a%20b.cmd` | ran | ran |
/// | `a b.cmd` | did not run | ran |
///
/// `start` is why the empty quotes are there: its first quoted argument is the new window's
/// title, so without them a URL is read as the program to run. The bug this replaced was not
/// the empty quotes -- those were right -- but the URL: as an ordinary argument it is quoted
/// only when it contains a space, and a URL that is not quoted is a command line.
fn browser_invocation(url: &str) -> flint::tools::Invocation {
    if cfg!(target_os = "windows") {
        flint::tools::Invocation {
            program: "cmd".to_string(),
            args: vec!["/S".to_string(), "/C".to_string()],
            raw: Some(format!("\"start \"\" \"{url}\"\"")),
        }
    } else if cfg!(target_os = "macos") {
        flint::tools::Invocation::plain("open", vec![url.to_string()])
    } else {
        flint::tools::Invocation::plain("xdg-open", vec![url.to_string()])
    }
}

/// Say where the view is, and open it when there is somebody there to look at it.
///
/// One function for `--web`, `/web` and `--web` typed at the prompt, because they are the same
/// fact and three printings of it would drift. The URL goes to the transcript first, so it is
/// selectable even in the case where the browser never appears.
fn announce_view(printer: &Printer<'_>, url: &str) {
    let dim = printer.pal.dim;
    let reset = printer.pal.reset;
    let opened = std::io::stdout().is_terminal() && open_in_browser(url);
    if opened {
        printer
            .term()
            .line(format_args!("{dim}web:{reset} {url}{dim}  (opening it){reset}"));
    } else {
        printer.term().line(format_args!("{dim}web:{reset} {url}"));
    }
}

/// A command-line flag typed at the prompt, and what to do about it.
///
/// Reported as a bug in exactly this shape, twice over: `--web` entered at the prompt, because
/// it is the only name for the feature a person has met -- it is in `--help`, in the README and
/// in this program's own error messages -- and nothing marks it as belonging to the command
/// line rather than to the conversation. It went to the model, which answered it politely and
/// at length, and the run looked like it had worked.
///
/// **So a flag at the prompt is translated, not refused.** `--provider x` becomes `/provider x`,
/// which is what was meant. Refusing it with an explanation was the first version and was the
/// wrong answer: the person had already said what they wanted, and being told to say it again
/// in a different spelling is not help.
///
/// **Only an exact flag is caught, and that is the whole design.** `--web` alone is a typo;
/// `why does --web need a token?` is a question and reaches the model untouched. A rule over
/// anything starting with `-` was the obvious version and is wrong: a pasted bullet list starts
/// that way, and swallowing it would be a worse bug than the one this fixes.
fn flag_at_the_prompt(input: &str) -> Option<(String, String)> {
    let mut words = input.split_whitespace();
    let flag = words.next()?;
    let one = words.next();
    if words.next().is_some() {
        return None;
    }
    let number = one.is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
    let named = one.is_some_and(|v| !v.is_empty() && !v.starts_with('-'));
    let (command, why) = match flag {
        "--web" if one.is_none() => ("/web".to_string(), "that flag opened it at start-up"),
        "--port" if one.is_none() || number => (
            format!("/web{}", one.map(|p| format!(" {p}")).unwrap_or_default()),
            "--web takes the port",
        ),
        "--provider" if one.is_none() || named => (
            format!("/provider{}", one.map(|p| format!(" {p}")).unwrap_or_default()),
            "that flag chose one at start-up",
        ),
        "--model" if one.is_none() || named => (
            format!("/model{}", one.map(|m| format!(" {m}")).unwrap_or_default()),
            "that flag chose one at start-up",
        ),
        "--readonly" if one.is_none() => ("/readonly".to_string(), "that flag is a toggle"),
        "--verbose" if one.is_none() => ("/verbose".to_string(), "that flag is a toggle"),
        "--help" | "-h" if one.is_none() => ("/help".to_string(), "this is the list"),
        "--list-sessions" if one.is_none() => ("/sessions".to_string(), "this is the list"),
        "--resume" if named => (
            format!("/resume {one}", one = one.unwrap_or_default()),
            "this picks by number",
        ),
        "--name" if named => (
            format!("/name {one}", one = one.unwrap_or_default()),
            "this names the open conversation",
        ),
        "--continue" if one.is_none() => (
            "/sessions".to_string(),
            "this lists them numbered, and `/resume <n>` picks one",
        ),
        // No equivalent, because there is nothing to be equivalent to: these decide how the
        // process is set up, and a process that is already running cannot be set up again.
        "--json" | "--no-color" | "--cwd" | "--schema" | "--no-schema" | "-p" => (
            String::new(),
            "that one is only read when flint starts, so it has to be on the command line",
        ),
        _ => return None,
    };
    Some((command, why.to_string()))
}

/// Whether a line typed while the model is working is meant for the model at all.
///
/// `run_turn` is where a line arriving mid-turn is picked up, and it has no way to run anything:
/// it takes the line and makes it the next prompt. So `/resume`, typed or clicked in the
/// browser's sidebar, became a *message* to the model while a turn was running -- and the moment
/// you want to look at another conversation is while one is churning. Measured against a slow
/// stub: `/resume 1` went out as a prompt and `/session` never changed.
///
/// The three forms are the three the REPL itself treats as more than text, and they are listed
/// here rather than inferred because this function and the REPL have to agree: a line this says
/// is for the REPL must be one the REPL will actually act on, or it would be swallowed.
fn for_the_repl(line: &str) -> bool {
    line.starts_with('/') || line.starts_with('!') || flag_at_the_prompt(line).is_some()
}

/// The channel a browser message arrives on, adapted into the one the keyboard feeds.
///
/// **This is the whole of what makes the page a composer.** A line typed into the browser
/// becomes `InputMsg::Line`, which is the event a keystroke produces, so it arrives at the
/// prompt, it steers a turn that is already running, and every slash command works from the
/// page without the page knowing that slash commands exist. `/resume 3` from the sidebar is
/// that and nothing more: the sidebar sends text, the REPL decides what text starting with `/`
/// means, and the browser is not a second place that decides.
///
/// `Some` only when there is a prompt to type into. A one-shot `-p` run has an input channel it
/// never reads -- `can_steer` is false -- so a message posted to it would be accepted and then
/// silently dropped, which is the failure this project keeps designing against. `None` makes
/// `POST /message` answer `409` instead.
fn browser_input(
    reader: &InputReader,
    has_prompt: bool,
) -> Option<tokio::sync::mpsc::UnboundedSender<web::FromPage>> {
    if !has_prompt {
        return None;
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<web::FromPage>();
    let line_tx = reader.line_tx.clone();
    tokio::spawn(async move {
        while let Some(what) = rx.recv().await {
            // The kind is carried across, not flattened into a line: see `InputMsg::Report`, and
            // note that this is the *only* place the browser's vocabulary meets the keyboard's.
            let msg = match what {
                web::FromPage::Line(text) => InputMsg::Line(text),
                web::FromPage::Report(text) => InputMsg::Report(text),
            };
            if line_tx.send(msg).is_err() {
                break;
            }
        }
    });
    Some(tx)
}

/// Ask a question and wait for a line.///
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
    // Which provider is in force, so that switching to the one just configured also leaves
    // the one it replaces. Carried in rather than read off the agent, which knows its model
    // and not which entry it came from.
    from: Option<&str>,
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
        // The engine commands are not something the wizard asks about — it is a form for
        // reaching an endpoint, and these are about running a program. Carrier over from the
        // provider being edited, so correcting a URL does not quietly stop flint from
        // starting the engine that URL depends on.
        start: existing.as_ref().and_then(|e| e.start.clone()),
        stop: existing.as_ref().and_then(|e| e.stop.clone()),
        start_timeout_secs: existing.as_ref().map(|e| e.start_timeout_secs).unwrap_or(0),
        // Inherited from the config's shell proxy, which is what a user setting up a
        // provider behind one has already told us.
        proxy: cfg.proxy.clone(),
    };

    save_provider(cfg, &p, printer)?;

    if confirm(reader, &format!("switch to '{name}' now?"), true).await? {
        cfg.default_provider = name.clone();
        cfg.save()?;
        return switch_provider(cfg, from, &name, agent, printer.term(), printer.pal).await;
    }
    Ok(Flow::Continue)
}

/// Write a provider into the config file and say what happened to it.
///
/// Shared by the wizard and by `/provider add`'s one-line form, because the two differ only in how
/// the answers arrive. What is left here is the half that can go wrong quietly: a name that replaces
/// an existing provider, and an endpoint that needs a key and does not have one. Both are said out
/// loud, and the second one names the command that fixes it.
///
/// The caller does the switching, because the wizard asks before it does and a line cannot ask.
fn save_provider(
    cfg: &mut config::Config,
    p: &config::ProviderConfig,
    printer: &Printer<'_>,
) -> Result<()> {
    let Palette { bold, reset, .. } = printer.pal;
    let had_key = !p.resolved_key().trim().is_empty();
    let added = cfg.upsert_provider(p.clone());
    cfg.save()?;
    printer.term().line(format_args!(
        "{} {bold}{}{reset} {} {}",
        printer.style(GREEN, "saved"),
        p.name,
        if added { "added to" } else { "updated in" },
        config::config_path().display()
    ));
    if !had_key && !is_local_endpoint(&p.base_url) {
        printer.term().line(format_args!(
            "{}",
            printer.style(
                YELLOW,
                "  no key yet — set one with /provider key, or the request will fail"
            )
        ));
    }
    Ok(())
}

/// Hand a conversation to a new agent, in the file it is already in.
///
/// `/model`, `/provider` and `/reload` all replace the agent -- a different model, a different
/// endpoint, or a config that was just edited -- and all three used to build the replacement
/// empty: a fresh history *and* a brand new file, while the transcript on screen went on showing
/// the conversation they had just dropped. Measured, with `/resume` on a session holding a
/// question and an answer, then `/model <other>`, then one line: the request that went out had
/// two messages, the system prompt and the new line. Nothing fails and nothing says so; the model
/// simply answers as a stranger, which is the same complaint as a stopped turn losing the answer
/// it had drawn, through a different door.
///
/// The history came back then and the file did not, which was the second half of the same mistake
/// in a quieter form: the replacement agent seeded a **new** session with the whole conversation
/// copied into it, so switching provider split one conversation into two files with the same
/// messages. The page grew a sidebar row nobody asked for, `/sessions` numbered the same
/// conversation twice, `/delete` on one of them left the other behind, and resuming either half
/// resumed half a conversation. The file is append-only and the run keeps writing where it was:
/// the agent is replaced, the conversation is not, and a `switch` event records the model it moved
/// to so `--resume` still believes the file. `/new` and `/resume` are the two that really do move,
/// and neither comes through here.
///
/// The messages go back through `splice_loaded_history` rather than being assigned, which is the
/// route `/resume` takes and for the same reason: a session file holds the conversation and not
/// the system prompt, and the prompt is rebuilt for the machine flint is on now.
fn continue_conversation(
    cfg: &config::Config,
    provider: provider::Provider,
    target: &config::ProviderConfig,
    old: &agent::Agent,
) -> Result<agent::Agent> {
    let cwd = old.cwd().clone();
    let writer = match old.session_path().filter(|path| path.exists()) {
        Some(path) => session::SessionWriter::resume(&path)?,
        // A session that has said nothing yet has no file -- sessions are created by their first
        // event -- so continuing is starting: the same conversation, still with nothing in it.
        None => session::SessionWriter::create(
            &config::sessions_dir(),
            &cwd,
            &target.name,
            &target.model,
        )?,
    };
    // The move is recorded either way, and on a session with no file yet this is the write that
    // makes the file. It has to be: leaving it out of one branch is how a switch came to write
    // nothing at all, with the page told the provider had changed and no file to prove it. That it
    // repeats what `meta` already says on a brand-new session is the price of one code path, and
    // `load` believes the last line either way.
    writer.switched(&target.name, &target.model)?;
    let mut next = agent::Agent::new(cfg, provider, old.readonly(), cwd.clone(), Some(writer));
    next.splice_loaded_history(cfg, &cwd, old.history().to_vec());
    Ok(next)
}

/// Build an agent for `name` and hand it back to the REPL.
///
/// The conversation and the two things that say how commands run -- the working directory and
/// the read-only guard -- are carried over from the running agent rather than re-derived, so
/// switching provider does not silently change where commands run or drop the guard.
///
/// This is also where engines are dealt with, because a switch is the only moment either
/// half of that makes sense: arriving somewhere is a chance to start what is not running,
/// and leaving is a chance to free what is. See [`engine`].
async fn switch_provider(
    cfg: &config::Config,
    from: Option<&str>,
    name: &str,
    old: &agent::Agent,
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

    // The order is the point: the engine being moved *to* comes up before the one being
    // moved *from* goes down. The other way round is tidier about memory and much worse
    // about failure -- a start that times out would leave the user with neither engine
    // running and the old provider already stopped, which is a state they cannot get out of
    // without reading this source file.
    let says = |line: String| term.line(format_args!("{dim}{line}{reset}"));
    if let Err(e) = engine::ensure_up(&target, cfg, says).await {
        // Reported and not fatal: the provider the user asked for is still the provider they
        // get, and they may well have started the engine themselves a moment ago. What must
        // not happen is a switch that silently did nothing.
        term.line(format_args!(
            "{yellow}warning:{reset} {e:#}
{dim}  switching anyway — the first message will \
             be the thing that fails if the engine is not up.{reset}"
        ));
    }
    if let Some(previous) = from.filter(|previous| *previous != name) {
        if let Some(previous) = cfg.provider(previous) {
            if let Err(e) = engine::shut_down(previous, cfg, |line| {
                term.line(format_args!("{dim}{line}{reset}"))
            })
            .await
            {
                term.line(format_args!("{yellow}warning:{reset} {e:#}"));
            }
        }
    }

    let provider = provider::Provider::new(target.clone())?;
    // The conversation comes with it: it is the same conversation on a different model, and the
    // transcript above the prompt is still showing it.
    let new_agent = continue_conversation(cfg, provider, &target, old)?;
    term.line(format_args!(
        "switched to {bold}{}{reset} ({})",
        target.name, target.model
    ));
    Ok(Flow::NewAgent(new_agent, target))
}

/// Which block of `/help` a row belongs to.
#[derive(Clone, Copy, PartialEq)]
enum HelpSection {
    /// The commands themselves.
    Commands,
    /// The one line that is not a command: what typing during a turn does, which `/stop` is the
    /// short spelling of.
    Working,
}

/// What the page may draw for a command, if anything.
///
/// §8's classes, and the reason every row carries one. The frame hands the page a menu, and a
/// control drawn for a command the terminal refuses is the failure the toggle round was built to
/// prevent; the class is also what keeps this program's grammar out of the page -- a form needs an
/// input, a destructive command needs confirming, a report needs nothing -- and it is what
/// `tests/cli_output.rs` filters on, because running `/delete` to find out whether it exists is not
/// a check anybody wants.
#[derive(Clone, Copy, PartialEq)]
enum OnPage {
    /// A report. §8's first class: the page shows the listing rather than sending the command,
    /// since the terminal is where somebody asked for it and the page's reader did not.
    Panel,
    /// One action, no argument.
    Button,
    /// A setting whose value is already somewhere else -- `/model`'s and `/provider`'s in the
    /// `providers` field of this same frame, `/resume`'s in the sidebar's rows.
    Selector,
    /// Free-form input.
    Form,
    /// Destroys something. A button, behind the page's own confirmation: nothing in flint is
    /// recoverable, and a misclick in a browser is a misclick.
    Danger,
    /// Already on the page from another field of the frame. Sending these in the command list as
    /// well would be one fact in two places, which is how the two come to disagree about it.
    Toggles,
    /// The terminal's own, and not offered on the page at all: `/exit` because the page is a window
    /// onto a process and a misclick must not end a session, `/web` because the page *is* the web
    /// view, `!` because it is a shell escape the composer can type anyway, `/stop` because the
    /// composer already has the button.
    Terminal,
}

/// Where the page gets the argument of a destructive row from.
///
/// §8 says a selector's values come from a frame that already describes them rather than from the
/// command list, and that holds for `/resume` and `/model`: the control decides. A destructive row is
/// the exception, because the page has to draw the *choices* before anything can be confirmed -- and
/// the two it has are different lists, so a page that could not tell `/delete <n|id>` from
/// `/provider rm <name>` would have to recognise the commands by name, which is the one thing it must
/// never do. So the row says which list, and the page offers the candidates it already holds for
/// that list: the sidebar's numbers for a conversation, the picker's names for a provider.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgFrom {
    /// The conversations the sidebar is already showing, by the number it shows them with.
    Sessions,
    /// The providers the state frame is already carrying, by name.
    Providers,
}

impl ArgFrom {
    fn word(self) -> &'static str {
        match self {
            ArgFrom::Sessions => "sessions",
            ArgFrom::Providers => "providers",
        }
    }
}

/// The kind of value a row's argument is, when the page draws a field for it.
///
/// A row with no arguments at all gets no field, which is how `/provider edit` and `/config edit`
/// stay where they are: a page drawing a field for them would be guessing at the rest.
/// `/provider add` used to be in that list and is not any more -- it takes its answers as the words
/// of one line now, so a page can ask for each of them -- and `/config edit` is the one still worth
/// revisiting: its keys are enumerable and only its values are free-form, and a page form for it
/// would need a `/config set <key> <value>` the terminal does not have, which is a command invented
/// for the page's benefit rather than one it was taught.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    /// One line of text, shown as it is typed.
    Text,
    /// A credential: drawn masked, cleared once sent, and never repeated in a frame. See
    /// `echoed_input`, which is the other half of that promise.
    Password,
}

impl Field {
    /// The word this kind is carried as, which is also the `type` attribute of the input element.
    fn word(self) -> &'static str {
        match self {
            Field::Text => "text",
            Field::Password => "password",
        }
    }
}

/// One answer a row's line is made of, as the page is told about it.
///
/// A row may take more than one answer -- `/provider add` takes a name, an address and a model --
/// and the page's job with them is the one thing a single field could not do: ask for each, and
/// refuse to send at all while one the frame did not mark optional is empty. That refusal is not
/// politeness. `send` with no answers is the *interactive* form of the command, so a page that sent
/// an empty one would hand the run to a wizard waiting for a person who is not in the room.
#[derive(Clone, Copy)]
struct PageArg {
    /// What the answer is, which is the input's placeholder: `base_url`.
    ///
    /// Not a label: on a row with one answer the help is what the field suggests (it says what the
    /// command is *for*), and on a row with several each field has to say which answer it wants,
    /// because there is no room to explain three of them one at a time.
    name: &'static str,
    /// How to draw it.
    field: Field,
    /// True when the command works without this answer.
    ///
    /// A line cannot leave a hole in the middle -- the words are positions -- so an optional answer
    /// has to be the last one, which is why `/provider add`'s model is the only one marked.
    optional: bool,
}

impl PageArg {
    const fn required(name: &'static str, field: Field) -> Self {
        Self {
            name,
            field,
            optional: false,
        }
    }

    const fn optional(name: &'static str, field: Field) -> Self {
        Self {
            name,
            field,
            optional: true,
        }
    }
}

/// One row of `/help`, and one row of what the page is offered.
///
/// There are three readers of this fact -- the terminal's help, the REPL's dispatch, and the page's
/// menu -- and the point of the table is that there is one copy of it. A second copy is how a page
/// comes to offer a command that has been renamed while `/help` goes on describing the old one, and
/// how a button for a command that takes an argument ends up sending a line with none in it.
struct CommandHelp {
    /// What `/help` prints in the left column: the word, and what it takes.
    label: &'static str,
    /// The line the page sends for it, with no argument: `/provider add`, `/name`.
    ///
    /// Kept beside the label instead of split out of it, because the split is not uniform -- `add`
    /// is part of the command and `<key>` is not -- and a page that had to tell those apart would
    /// be re-deriving this program's own grammar.
    send: &'static str,
    /// One line about what it does. The printer wraps it; the page gets it as written.
    help: &'static str,
    section: HelpSection,
    on_page: OnPage,
    /// The answers the page may fill in for this row, in the order the line wants them.
    ///
    /// Empty for every row whose argument the page may not type, which is how a row becomes a row of
    /// reference: the page draws a control for what it is given, and a line to read for what it is
    /// not.
    args: &'static [PageArg],
    /// Set on a destructive row: which list the page offers the argument from. See [`ArgFrom`].
    from: Option<ArgFrom>,
}

impl CommandHelp {
    /// A row of the table, in one line, with the columns in the order the struct declares them.
    const fn row(
        label: &'static str,
        send: &'static str,
        help: &'static str,
        section: HelpSection,
        on_page: OnPage,
    ) -> Self {
        Self {
            label,
            send,
            help,
            section,
            on_page,
            args: &[],
            from: None,
        }
    }

    /// A row whose one argument the page may type into a field.
    ///
    /// A separate constructor rather than a `row` with another argument, because a field is the
    /// exception -- three rows have one, and every other row would be passing an empty list to say
    /// so. The list is passed in rather than built here, because `&[PageArg { .. }]` inside a
    /// `const fn` is a temporary: const promotion does not reach into a function body, so these are
    /// named constants beside the table.
    const fn field_row(
        label: &'static str,
        send: &'static str,
        help: &'static str,
        section: HelpSection,
        on_page: OnPage,
        args: &'static [PageArg],
    ) -> Self {
        Self {
            label,
            send,
            help,
            section,
            on_page,
            args,
            from: None,
        }
    }

    /// A row the page asks several answers for, which it sends as the words of one line.
    ///
    /// The arguments are the table's, not the page's: `args[0]` is the first word after `send`, and
    /// the page is told which of them may be left empty rather than deciding from the label. This is
    /// `/provider add`, and the reason it needed one: the interactive form of it asks for five
    /// answers one at a time, and a page cannot hold a conversation.
    const fn form_row(
        label: &'static str,
        send: &'static str,
        help: &'static str,
        args: &'static [PageArg],
    ) -> Self {
        Self {
            label,
            send,
            help,
            section: HelpSection::Commands,
            on_page: OnPage::Form,
            args,
            from: None,
        }
    }

    /// A row that destroys something, whose argument the page offers from a list it already holds.
    ///
    /// The separate constructor for the same reason as the field's, and one more: this is the class
    /// where a wrong line is unrecoverable, so it is worth a row that cannot be written by accident.
    const fn destroying(
        label: &'static str,
        send: &'static str,
        help: &'static str,
        from: ArgFrom,
    ) -> Self {
        Self {
            label,
            send,
            help,
            section: HelpSection::Commands,
            on_page: OnPage::Danger,
            args: &[],
            from: Some(from),
        }
    }
}

/// Every command the REPL takes, in the order `/help` has always printed them.
///
/// Aliases are not rows: `/q` and `/quit` are dispatched, but three spellings of one command is
/// three lines of help for one thing, and the page has no use for them.
/// The answers of the rows that take exactly one, as constants rather than expressions in the table.
///
/// A `&[PageArg]` built inside a `const fn` is a temporary that borrows from the frame, which the
/// compiler refuses; naming them here is what makes the table one list instead of three.
const NAME_ARG: [PageArg; 1] = [PageArg::required("text", Field::Text)];
const KEY_ARG: [PageArg; 1] = [PageArg::required("key", Field::Password)];
/// `/provider add`'s three: everything a provider needs that is not a secret.
///
/// The model is optional because a provider can be added without one -- the terminal's default is
/// `deepseek-chat`, the same as the wizard's -- and it is the last one because a line cannot leave a
/// hole in the middle.
const ADD_ARGS: [PageArg; 3] = [
    PageArg::required("name", Field::Text),
    PageArg::required("base_url", Field::Text),
    PageArg::optional("model", Field::Text),
];

const COMMANDS: &[CommandHelp] = &[
    CommandHelp::row("/help", "/help", "this message", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/exit", "/exit", "quit", HelpSection::Commands, OnPage::Terminal),
    CommandHelp::row("/provider", "/provider", "list providers", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/provider <name>", "/provider", "switch to one", HelpSection::Commands, OnPage::Selector),
    CommandHelp::form_row(
        "/provider add <name> <base_url> [model]",
        "/provider add",
        "set up a new provider; with no arguments it asks for each part",
        &ADD_ARGS,
    ),
    CommandHelp::row("/provider edit <name>", "/provider edit", "change one (interactive)", HelpSection::Commands, OnPage::Form),
    CommandHelp::field_row("/provider key <key>", "/provider key", "set the API key for the active provider", HelpSection::Commands, OnPage::Form, &KEY_ARG),
    CommandHelp::destroying("/provider rm <name>", "/provider rm", "delete one", ArgFrom::Providers),
    CommandHelp::row("/config", "/config", "show shell, steps, proxy", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/config edit", "/config edit", "change shell, steps, proxy", HelpSection::Commands, OnPage::Form),
    CommandHelp::row("/model", "/model", "show the model in force", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/model <name>", "/model", "switch to one", HelpSection::Commands, OnPage::Selector),
    CommandHelp::row("/usage", "/usage", "context and token accounting", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/verbose [on|off|full]", "/verbose", "how much of the agent's activity to narrate", HelpSection::Commands, OnPage::Toggles),
    CommandHelp::row("/detail [on|off]", "/detail", "print tool output (off: one line per result)", HelpSection::Commands, OnPage::Toggles),
    CommandHelp::row("/readonly [on|off]", "/readonly", "toggle the write guard", HelpSection::Commands, OnPage::Toggles),
    CommandHelp::row("/tools", "/tools", "list available tools", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/skills [name]", "/skills", "list skills, or print one as the model would see it", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/sessions", "/sessions", "list past sessions, numbered", HelpSection::Commands, OnPage::Panel),
    CommandHelp::row("/resume <n|id>", "/resume", "switch to one of them", HelpSection::Commands, OnPage::Selector),
    CommandHelp::field_row("/name [text]", "/name", "name this conversation", HelpSection::Commands, OnPage::Form, &NAME_ARG),
    CommandHelp::destroying("/archive <n|id>", "/archive", "file one away, out of the list", ArgFrom::Sessions),
    CommandHelp::destroying("/delete <n|id>", "/delete", "delete one", ArgFrom::Sessions),
    CommandHelp::row("/new", "/new", "start a fresh conversation", HelpSection::Commands, OnPage::Button),
    CommandHelp::row("/web [port]", "/web", "open the browser view of this conversation", HelpSection::Commands, OnPage::Terminal),
    CommandHelp::row("/reload", "/reload", "re-read the config file (after editing it yourself)", HelpSection::Commands, OnPage::Button),
    CommandHelp::row("!<command>", "!", "run a shell command without the model", HelpSection::Commands, OnPage::Terminal),
    CommandHelp::row("/stop", "/stop", "the same thing as one short word, for when a key is not available -- ssh, a pipe, or the browser's composer.", HelpSection::Working, OnPage::Terminal),
];

/// Where a description starts: two columns of indent, the label column, and the space after it.
///
/// Written down rather than measured from the table, because this is the column `/help` has always
/// used: the table arrives to fill it, not to re-flow it. A label wider than the column spills one
/// space past its own description's column, which the widest row has always done.
const HELP_COLUMN: usize = 24;
/// The width a label is padded to, which is the column less the space that separates it.
const HELP_LABEL: usize = HELP_COLUMN - 3;
/// How much room a description gets before it wraps: 80 columns less its own column.
///
/// The width the hand-written help was wrapped to, kept so that moving the rows into a table does
/// not silently re-flow every one of them.
const HELP_ROOM: usize = 80 - HELP_COLUMN;

/// `/help`: the table, printed in the two blocks it has always been printed in.
///
/// The rows come from `COMMANDS` and the prose does not, which is the split that keeps the help and
/// the page's menu together: a command that exists is a row, and everything else here explains a
/// *block* of rows rather than a command.
fn print_help_table(printer: &Printer<'_>) {
    let Palette { dim, reset, .. } = printer.pal;
    let line = |text: std::fmt::Arguments<'_>| printer.term().line(text);

    line(format_args!("{dim}commands{reset}"));
    help_rows(printer, HelpSection::Commands);
    line(format_args!("{dim}while the model is working{reset}"));
    line(format_args!(
        "  Type and press Enter to interrupt it. Your line becomes the next input."
    ));
    help_rows(printer, HelpSection::Working);
    line(format_args!("{dim}notes{reset}"));
    line(format_args!(
        "  Permission model is full by default. /readonly is the only guard."
    ));
    line(format_args!(
        "  Everything above is configurable from inside flint; the file is only there"
    ));
    line(format_args!(
        "  so that it stays hand-editable when that is easier."
    ));
    line(format_args!(
        "  Config file: {reset}{}",
        config::config_path().display()
    ));
}

/// One block of the table: every row in this section, each wrapped under its own column.
fn help_rows(printer: &Printer<'_>, section: HelpSection) {
    for row in COMMANDS.iter().filter(|row| row.section == section) {
        let mut wrapped = wrap(row.help, HELP_ROOM).into_iter();
        let first = wrapped.next().unwrap_or_default();
        printer
            .term()
            .line(format_args!("  {:<HELP_LABEL$} {}", row.label, first));
        for rest in wrapped {
            printer
                .term()
                .line(format_args!("{:HELP_COLUMN$}{rest}", ""));
        }
    }
}

/// Greedy word wrap: break before the word that would pass `room`.
///
/// The breaks the help has always had, done by hand until now, including the continuation of its
/// widest row -- so keeping the rule keeps the breaks.
fn wrap(text: &str, room: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let grown = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if grown > room && !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    out
}

/// Answer a report the page asked for: run the command with this terminal quiet, and send what it
/// said to the page's panel instead.
///
/// §8's panel class. A page shows a listing in a panel of its own rather than sending it into the
/// transcript, because this terminal is where somebody typed `/help` and the page's reader did not
/// ask for it here. So the answer is recorded rather than drawn, which is the whole of the
/// difference between this and a line typed into the composer -- the same channel, the same
/// dispatch, and one flag.
///
/// The check that the command really is a report lives *here*, beside the table, rather than in the
/// route: `web.rs` does not know what a slash command means, and keeping that true is worth more
/// than a whitelist in the one file that is proudest of not having one. It is also the safety half.
/// The page has no confirmation step yet, so a report route that ran whatever it was handed would be
/// a way to delete a conversation with one click that never happened.
async fn run_report(
    asked: &str,
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &mut config::ProviderConfig,
    printer: &Printer<'_>,
    reader: &InputReader,
    viewer: &mut Option<web::Viewer>,
) {
    let asked = asked.trim();
    // What the page may read: a line its own menu offers as a report. The bare `send` of a panel row
    // is that report; a row that carries `values` may also be asked with one of them, which is how
    // `/skills <name>` reaches the panel. Matched against the *frame's own rows* rather than the
    // table, because the frame is what the page was shown -- and because the values are part of the
    // permission: `/provider <name>` and `/model <name>` take values too, and running those quietly
    // would switch provider or model with nothing printed anywhere. They are not reports because
    // their class is not `panel`: the page types those lines instead, which is what the class is for.
    let is_report = page_rows(cfg, provider_cfg, agent).iter().any(|row| {
        row.class == "panel"
            && (row.send == asked
                || row
                    .values
                    .iter()
                    .any(|value| format!("{} {value}", row.send) == asked))
    });
    if !is_report {
        report_refused(asked, printer, viewer);
        return;
    }

    printer.term().quiet_start();
    let flow = handle_command(asked, cfg, agent, provider_cfg, printer, reader, viewer).await;
    let mut said = printer.term().quiet_take();

    match flow {
        // A report is a read, so "carry on" is the only flow it can come back as.
        Ok(Flow::Continue) => {}
        // Anything else would mean the table calls a command a report that is not one, and the page
        // is told rather than left waiting.
        Ok(_) => {
            report_refused(asked, printer, viewer);
            return;
        }
        // A read can *fail* -- a skill whose file went away between the menu being drawn and the row
        // being pressed -- and the failure is the answer: the page asked to read this, so the error
        // belongs in the panel it opened rather than nowhere. Through the same channel as the text,
        // which is also what keeps a failed read from printing on this terminal.
        Err(e) => said.push(format!("error: {e:#}")),
    }
    if let Some(live) = viewer.as_ref().map(web::Viewer::live) {
        live.report(asked, &said);
    }
}

/// A page asked for something that is not a report: said on the terminal and sent back to the page.
///
/// Drawn here rather than through the quiet path, and that is the point of it: this line is about
/// the *request*, not about a listing, and a page that asked for something it may not have should
/// not be able to make this terminal print anything. The page is told too, in the panel it opened,
/// or it would sit waiting for an answer that is never coming.
///
/// The line is echoed through `echoed_input` like any other answer: a refusal names what was asked
/// for, and a page that posted `/provider key <key>` to the read route -- which is refused, because
/// reading is not what that command does -- would otherwise have its key printed here and sent to
/// every other page watching.
fn report_refused(asked: &str, printer: &Printer<'_>, viewer: &Option<web::Viewer>) {
    let shown = echoed_input(asked);
    let said = format!("{shown} is not a report -- the page may read only the commands its own menu offers, with a value it carries");
    printer.term().line(format_args!(
        "{} {}",
        printer.style(RED, "refused:"),
        printer.dim(&said)
    ));
    if let Some(live) = viewer.as_ref().map(web::Viewer::live) {
        live.report(shown, &[said]);
    }
}

async fn handle_command(
    input: &str,
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &mut config::ProviderConfig,
    printer: &Printer<'_>,
    reader: &InputReader,
    viewer: &mut Option<web::Viewer>,
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
            // The list is the table every other reader of it uses -- see `COMMANDS`. Aliases are
            // dispatched a few lines down (`/quit`, `/q`, and `/provider`'s sub-words), and are
            // deliberately not rows: three spellings of one command is three lines of help for one
            // thing.
            print_help_table(printer);
        }

        // The browser view, opened from inside a conversation.
        //
        // This is the way a person actually reaches for it: `--web` has to be decided before
        // the run starts, and the moment you want a real renderer is thirty seconds into an
        // answer that is scrolling past faster than you can read. Reported as a bug in
        // exactly that shape -- `--web` typed at the prompt, which is a flag and not a
        // command, so it went to the model and came back as a sentence about web modes.
        "/web" => {
            let port = match arg {
                "" => 0,
                _ => match arg.parse::<u16>() {
                    Ok(port) => port,
                    Err(_) => {
                        printer.term().line(format_args!(
                            "{red}/web takes a port number or nothing{reset} — got {arg:?}"
                        ));
                        return Ok(Flow::Continue);
                    }
                },
            };
            let browser = browser_input(reader, true);
            let asked = viewer
                .get_or_insert_with(|| web::Viewer::asked(0, agent.session_path(), browser));
            match asked.open(port).await {
                Ok(url) => announce_view(printer, &url),
                // Not fatal, unlike `--web`: there the view was the whole point of the run,
                // and here it is one thing the user asked for that did not work out. Taking
                // a conversation down over a busy port would be the worse answer.
                Err(e) => {
                    printer.term().line(format_args!(
                        "{} {e:#}",
                        printer.style(RED, "web:")
                    ));
                }
            }
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
                        // What flint would do about this provider's engine, when it would
                        // do anything: a command derived from the name is invisible in the
                        // config, and this is where a person finds out it exists.
                        let engine = engine::describe(p);
                        let engine = if engine.is_empty() {
                            String::new()
                        } else {
                            format!("  {dim}{engine}{reset}")
                        };
                        printer.term().line(format_args!(
                            "  {mark} {:<12} {:<34} {}{key_state}{engine}",
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

                "add" => {
                    // Bare, the wizard asks for each answer in turn; with the answers already on the
                    // line there is nothing to ask, and that form exists so a page can send one. Both
                    // end in the same place -- `save_provider` -- because the half that can go wrong
                    // is not how the five answers arrived.
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.is_empty() {
                        return provider_wizard(cfg, agent, Some(&provider_cfg.name), None, printer, reader)
                            .await;
                    }
                    if parts.len() < 2 {
                        return Err(anyhow!(
                            "usage: /provider add <name> <base_url> [model] — or bare, to be asked each part"
                        ));
                    }
                    let (name, base_url) = (parts[0], parts[1]);
                    if cfg.provider(name).is_some() {
                        return Err(anyhow!(
                            "provider '{name}' already exists — `/provider edit {name}` changes one"
                        ));
                    }
                    let p = config::ProviderConfig {
                        name: name.to_string(),
                        base_url: base_url.to_string(),
                        // The two a line cannot carry without putting them in a transcript: a key is
                        // set afterwards with `/provider key`, which is masked and goes to whichever
                        // provider is in force -- which is why this switches to the new one below.
                        api_key: String::new(),
                        api_key_env: None,
                        models: Vec::new(),
                        // The wizard's own default, so a provider added from a page is the same shape
                        // as one added from the terminal.
                        model: parts.get(2).copied().unwrap_or("deepseek-chat").to_string(),
                        start: None,
                        stop: None,
                        start_timeout_secs: 0,
                        proxy: cfg.proxy.clone(),
                    };
                    save_provider(cfg, &p, printer)?;
                    // Switching is what the wizard offers as its default, and it is what makes the
                    // key reachable: `/provider key` sets the key for the provider *in force*.
                    cfg.default_provider = name.to_string();
                    cfg.save()?;
                    return switch_provider(cfg, Some(&provider_cfg.name), name, agent, printer.term(), printer.pal)
                        .await;
                }

                "edit" => {
                    if rest.is_empty() {
                        return Err(anyhow!("usage: /provider edit <name>"));
                    }
                    if cfg.provider(rest).is_none() {
                        return Err(anyhow!("unknown provider '{rest}'"));
                    }
                    return provider_wizard(cfg, agent, Some(&provider_cfg.name), Some(rest), printer, reader).await;
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
                        return switch_provider(cfg, Some(&provider_cfg.name), &fallback, agent, printer.term(), printer.pal).await;
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
                    return switch_provider(cfg, Some(&provider_cfg.name), &target.name, agent, printer.term(), printer.pal).await;
                }

                _ => {
                    let target = cfg
                        .provider(first)
                        .ok_or_else(|| {
                            anyhow!("unknown provider '{first}' (try /provider add or /provider)")
                        })?
                        .clone();
                    // Switching here makes it the default for the next run, which is what a
                    // person means by typing it.
                    cfg.default_provider = target.name.clone();
                    cfg.save()?;
                    // And then the switch itself is `switch_provider`'s job — the engines
                    // included. This branch used to build the agent inline, with its own
                    // copy of four lines that had to stay in step with that function; the
                    // copy is why `/provider <name>` was the one path that did not start or
                    // stop an engine, which is the path everybody uses.
                    return switch_provider(
                        cfg,
                        Some(&provider_cfg.name),
                        &target.name,
                        agent,
                        printer.term(),
                        printer.pal,
                    )
                    .await;
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
                let new_agent = continue_conversation(cfg, provider, &target, agent)?;
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

            // Saved first, so that what the next run reads agrees with what this one is about to
            // enforce. It did not, and worse: this arm used to work out the value it meant to set,
            // print it, and set nothing at all. `/readonly on` therefore reported a guard that did
            // not exist, in a tool whose manual calls this its only permission control -- a
            // command that says it changed something and did not is worse than one that fails.
            cfg.readonly = turn_on;
            cfg.save()?;

            // The tools are built *with* the flag, so changing it means building them again, and
            // on this conversation's own session file rather than a new one: this is a change of
            // permission, not of conversation, so the file, the history and the model stay put.
            // (`Flow::NewAgent` is the shape for it -- the loop re-points the page at the session
            // the new agent is writing, which here is the file it was already writing.)
            //
            // What it costs is what every rebuild costs, and it is the trade `ROADMAP.md` §9
            // records for `/model` and `/provider`: a fresh tool set has read nothing, so the
            // model may be asked to read a file it read a moment ago.
            if turn_on == agent.readonly() {
                printer.term().line(format_args!(
                    "{dim}note: {} says readonly = {}{reset}",
                    config::config_path().display(),
                    turn_on
                ));
                return Ok(Flow::Continue);
            }
            // A session that has said nothing yet has no file, and `/readonly` must not be what
            // creates one: this is a change of permission, not of conversation, so the run keeps the
            // writer it has and says nothing to disk.
            let writer = match agent.session_path().filter(|path| path.exists()) {
                Some(path) => Some(session::SessionWriter::resume(&path)?),
                None => None,
            };
            let cwd = agent.cwd().clone();
            let provider = provider::Provider::new(provider_cfg.clone())?;
            let mut next = agent::Agent::new(cfg, provider, turn_on, cwd.clone(), writer);
            next.splice_loaded_history(cfg, &cwd, agent.history().to_vec());
            printer.term().line(format_args!(
                "{dim}note: the tool set is rebuilt, so its read history starts over; {} now says \
                 readonly = {}{reset}",
                config::config_path().display(),
                turn_on
            ));
            return Ok(Flow::NewAgent(next, provider_cfg.clone()));
        }

        "/verbose" => {
            // The words come from `display::Verbosity`, which is also what the config stores and
            // what the page's switch is drawn from: one table, so a setting cannot be spellable
            // in one place and unspellable in another.
            let next = match arg {
                "" => {
                    if printer.verbosity() == NORMAL {
                        CHATTY
                    } else {
                        NORMAL
                    }
                }
                other => display::Verbosity::from_word(other)
                    .ok_or_else(|| anyhow!("expected on|off|full, got '{other}'"))?
                    .level(),
            };
            printer.set_verbosity(next);
            // The word, not `next >= CHATTY`: the file has to be able to say "off", which a bool
            // could not -- this line is what made `/verbose off` come back as `on` next run.
            cfg.verbose = display::Verbosity::from_level(next);
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
            printer.term().line(format_args!("  verbose          = {}", cfg.verbose.word()));
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
            let new_agent = continue_conversation(cfg, provider, &target, agent)?;
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
            // The list changed under the browser. The numbering in the sidebar *is* the
            // numbering `/resume` takes, so a list left showing the old ones points every row
            // below the one that went at the wrong conversation -- and clicking one would carry
            // on talking in it. Reported from a real session: history tidied in the terminal,
            // and the page still offering the deleted conversation.
            if let Some(viewer) = viewer.as_mut() {
                viewer.list_changed();
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
/// How many answers flint will ask for before giving up on a schema.
///
/// One, plus two repairs. Small on purpose: a model told twice, in its own words, what its answer got
/// wrong is not usually going to be told a third time, and every attempt is a whole turn that a
/// caller is waiting on. A caller who wants more can call again -- the failed answers are in the
/// session file, and `--resume` continues from them.
const SCHEMA_ATTEMPTS: usize = 3;

/// The prompt for a repair turn: what was asked for, what came back, and what was wrong with it.
///
/// The model's own words are quoted back rather than summarised, because the fix is usually in the
/// part a summary would drop -- a field name spelled two ways, a string where a number was wanted --
/// and because a model that sees its actual answer does not have to reconstruct it.
fn repair_prompt(previous: &str, errors: &[String]) -> String {
    let listed = errors
        .iter()
        .map(|e| format!("- {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "That answer does not match the schema. Fix it and answer again with the corrected JSON \
         object and nothing else.\n\n\
         What was wrong:\n{listed}\n\n\
         The answer you gave was:\n{previous}"
    )
}

/// The JSON in an answer, tolerating a code fence around it.
///
/// JSON mode returns the object bare, in every case measured. A fence still appears often enough to
/// be worth one line of forgiveness -- a fenced object is the right answer spelled with markdown, and
/// refusing it would spend a whole repair turn on punctuation -- but nothing else is repaired here:
/// the point of local validation is to ask for a real answer, not to guess at one.
fn json_in(answer: &str) -> Option<serde_json::Value> {
    let trimmed = answer.trim();
    let body = match trimmed.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or(rest);
            rest.strip_suffix("```").unwrap_or(rest).trim()
        }
        None => trimmed,
    };
    serde_json::from_str(body).ok()
}

/// How often a run that has said nothing says it is still working.
///
/// Five seconds: short enough that a caller waiting on a pipe hears from the run well inside any
/// sensible timeout, long enough that a ten-minute tool call adds a hundred short lines rather than
/// thousands. Not a setting -- a caller that wants a different rate is asking for a different
/// interface, and one more number in the config would be one more thing to get wrong.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the run is doing, for a heartbeat to repeat while nothing else is being said.
///
/// Shared between the turn and the clock beside it, because the turn cannot help: it is `await`ing a
/// tool, which is exactly the moment nobody can emit anything. The phrase is the last thing the
/// stream described rather than something invented here, so a caller that hears "running bash" knows
/// what it would have seen if the tool were talking.
struct Progress {
    started: std::time::Instant,
    doing: String,
    /// Whether the wait in force has been announced by a heartbeat yet.
    ///
    /// This is what `restarted` means on the wire: the first line about a wait says the clock starts
    /// here, and the repeats say the same wait is still going. True to begin with, because the run
    /// has been "thinking" since it started and nothing has said so.
    fresh: bool,
    /// Cleared when the turn is over, under the same lock the beat writes under: a status line
    /// after `turn.completed` would tell a caller the run was still working after it had stopped.
    running: bool,
}

impl Progress {
    fn new() -> Self {
        Progress {
            started: std::time::Instant::now(),
            doing: "thinking".to_string(),
            fresh: true,
            running: true,
        }
    }

    /// Name the step a line described.
    fn saw(&mut self, event: &event::Event) {
        let doing = match event {
            event::Event::Text(_) | event::Event::Reasoning(_) => "writing the answer".to_string(),
            event::Event::ToolStart { name, .. } => format!("running {name}"),
            event::Event::ToolResult { .. } => "thinking".to_string(),
            // The agent's own status wins: it knows more about what it is waiting for than this
            // frame can infer from the events around it.
            event::Event::Status { text, .. } => text.clone(),
            _ => return,
        };
        if doing != self.doing {
            self.doing = doing;
            self.fresh = true;
        }
    }
}

/// Say the run is still working, every [`HEARTBEAT`], until [`Progress::running`] is cleared.
///
/// The lock is held across the write on purpose: that is what makes "no heartbeat after the turn
/// ended" true rather than likely. The turn clears the flag under the same lock, so a beat is either
/// entirely before that or does not happen at all.
async fn beat_while_working(progress: std::sync::Arc<std::sync::Mutex<Progress>>) {
    let mut ticker = tokio::time::interval(HEARTBEAT);
    // Intervals fire immediately and then on schedule; the first tick is thrown away because a run
    // that has only just started has nothing to report.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let mut out = std::io::stdout();
        let mut progress = progress.lock().unwrap_or_else(|e| e.into_inner());
        if !progress.running {
            return;
        }
        // Taken, not read: the first beat about a wait starts the reader's clock, the rest say the
        // same wait is still going. In its own statement because the guard is borrowed immutably by
        // the frame and mutably by this, and one expression cannot hold both.
        let restarted = std::mem::replace(&mut progress.fresh, false);
        let _ = writeln!(
            out,
            "{}",
            ndjson::heartbeat(
                &progress.doing,
                restarted,
                progress.started.elapsed().as_secs(),
            )
        );
        let _ = out.flush();
    }
}

async fn run_json_turn(
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    provider_error: Option<&str>,
    prompt: &str,
    shaping: &Shaping,
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
        // `no_key` specifically: this check is the key one, and a caller that reads the code can tell
        // "a person must paste a key" from "a person must add funds" without reading either sentence.
        emit(ndjson::error_coded(&format!("{e:#}"), "no_key", false));
        // Nothing was asked of the model, and no retry of this question will change the answer: the
        // provider has to be fixed first.
        return Ok(EXIT_UNAVAILABLE);
    }

    // One turn, or as many as it takes to get an answer the schema accepts. The loop is here rather
    // than around the whole function so that everything before it -- the stream's opening lines, the
    // check that the provider can be reached at all -- happens exactly once, and so that a repair
    // reads on the stream as what it is: a second `turn.started`, with the corrected prompt in it.
    let mut asked = prompt.to_string();
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        if attempt > 1 {
            emit(ndjson::turn_started(&asked));
        }

        let mut sink = ndjson::Sink::new();
        // Taken from the sink *before* `message.completed` is built, not after the turn: `Done`
        // drains the accumulator (a second `Done` must not repeat the message), so the turn's answer
        // is gone the moment it has been reported -- which is the trap this line exists to miss.
        let mut answer = String::new();
        // The clock beside the turn. Started here, where the turn starts, so the seconds a caller
        // reads are the seconds it has been waiting; stopped below under the same lock, so the last
        // line of the stream is never a heartbeat.
        let progress = std::sync::Arc::new(std::sync::Mutex::new(Progress::new()));
        let beat = tokio::spawn(beat_while_working(std::sync::Arc::clone(&progress)));

        // What the caller can still say while the run is going. A one-shot run has no keyboard, so
        // `/stop` arrives on stdin -- the same word the REPL takes, and for the same reason: it is
        // the interrupt that works when there is no key to press, which is exactly the situation a
        // caller is in. Reading stdin here costs one blocked thread and changes nothing for a caller
        // that never writes to it, which is every caller that does not know this exists.
        let (steering_tx, mut steering) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { return };
                if steering_tx.send(line).is_err() {
                    return;
                }
            }
        });
        // A line that is not `/stop` is counted, not repeated. Two reasons, and the second is the one
        // that matters: a pipe into a run is not a private channel. Anything that arrives -- a diff, a
        // customer record, a token, a parent agent's own protocol -- would be echoed onto stdout in
        // full, and stdout is what a caller logs; the buffer would also grow with whatever was piped.
        // A count is still said out loud, because the other failure is silence: a caller that wrote a
        // line deserves to know it did nothing, without flint repeating what it wrote.
        let mut ignored = 0usize;
        let mut listening = true;
        let outcome = {
            let mut run = std::pin::pin!(agent.run(&asked, |event| {
                if matches!(event, event::Event::Done) {
                    answer = sink.answer_so_far().to_string();
                }
                {
                    // Poisoning is not a reason to lose the line: the lock only ever guards two
                    // fields, and the beat ignores a panic anyway.
                    let mut progress = progress.lock().unwrap_or_else(|e| e.into_inner());
                    progress.saw(&event);
                }
                if let Some(line) = sink.line(&event) {
                    emit(line);
                }
            }));
            loop {
                tokio::select! {
                    result = &mut run => break Some(result),
                    // `if listening` because a closed channel is ready for ever: without it, a
                    // caller that closed stdin (or that never had one) would spin this loop.
                    line = steering.recv(), if listening => match line {
                        Some(line) if line.trim() == "/stop" => break None,
                        Some(_) => ignored += 1,
                        None => listening = false,
                    },
                }
            }
        };
        {
            progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .running = false;
        }
        beat.abort();

        if ignored > 0 {
            emit(ndjson::warning(&format!(
                "ignored {ignored} line{} on stdin: a one-shot run has no next prompt to steer, and \
                 the only line it acts on is /stop. What was written is not repeated here -- a caller's \
                 pipe is not a private channel, and flint does not know what it was handed",
                if ignored == 1 { "" } else { "s" }
            )));
        }

        let Some(result) = outcome else {
            // The turn was dropped, so the agent loop never reached the code that turns a step's text
            // into a message -- and the answer stays on the caller's screen with no record of it
            // anywhere. Committed here, by the code that did the dropping, because the agent cannot
            // do it for itself after its future is gone. Same fault and same fix as the REPL's.
            agent.commit_drawn_answer();
            // Said out loud, and before the turn's end: a caller that asked for the stop knows it
            // asked, but a log read afterwards has to be able to tell this from a turn that finished
            // with nothing to say.
            emit(ndjson::warning(
                "stopped at your request -- the model is not running any more, and the answer above \
                 is what had been written",
            ));
            emit(ndjson::turn_completed(
                agent.last_usage(),
                ndjson::Outcome::Stopped,
            ));
            // No repair attempt and no validation: a caller that stopped the run is not waiting for
            // another one, and a half-written answer failing a schema is the expected outcome rather
            // than a problem to fix.
            return Ok(EXIT_INTERRUPTED);
        };

        // How the turn ended is the caller's business as much as the answer is: `incomplete` means
        // flint stopped asking and the text above is half of what it had to say. The code says the
        // same thing to a shell that does not read the stream, and an unfinished answer is not a
        // success even when a schema happens to accept it -- a caller acting on half an answer has
        // the fault the schema was there to prevent.
        let outcome = if agent.ran_out_of_steps() {
            ndjson::Outcome::Incomplete
        } else {
            ndjson::Outcome::Complete
        };
        let code = if outcome == ndjson::Outcome::Incomplete {
            EXIT_DATAERR
        } else {
            EXIT_OK
        };
        match result {
            Ok(()) => emit(ndjson::turn_completed(agent.last_usage(), outcome)),
            // The failure goes on the stream as well as the exit code: a caller that reads
            // stdout should not have to also read stderr to find out what happened. The cause is
            // read from the error itself when flint knows it -- the provider attaches its own
            // classification, which is the only place that can tell a rate limit from an empty
            // account -- and a failure with no classification says so by carrying no code, rather
            // than naming the wrong cause at the edge.
            Err(e) => {
                let failure = e.downcast_ref::<provider::ProviderFailure>();
                match failure {
                    Some(f) => emit(ndjson::error_coded(&format!("{e:#}"), f.code, f.retryable)),
                    None => emit(ndjson::error(&format!("{e:#}"))),
                }
                return Ok(exit_code_for(failure, provider_error.is_some()));
            }
        }

        let Some(schema) = &shaping.schema else {
            return Ok(code);
        };
        let errors = match json_in(&answer) {
            Some(value) => match schema.validate(&value) {
                problems if problems.is_empty() => {
                    emit(ndjson::result(&value, attempt));
                    return Ok(code);
                }
                problems => problems,
            },
            None => vec![format!(
                "the answer is not JSON at all, so no part of the schema could be checked: {}",
                first_line(&answer)
            )],
        };

        if attempt >= SCHEMA_ATTEMPTS {
            // No `result` line, ever, for an answer that did not pass: a caller reading that type
            // must be able to trust that what it holds is what the schema describes. The errors go
            // out as one `error`, last, so the exit code and the stream agree.
            emit(ndjson::error(&format!(
                "the answer did not match the schema after {attempt} attempts:\n{}",
                errors
                    .iter()
                    .map(|e| format!("- {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )));
            // No `result` line and a code that says why: there is an answer, and it is not one a
            // caller can use. `EX_UNAVAILABLE` would send it looking for a provider fault and
            // `EXIT_FAILURE` would tell it nothing at all.
            return Ok(EXIT_DATAERR);
        }
        asked = repair_prompt(&answer, &errors);
    }
}

/// The first line of an answer, for a message about an answer that could not be read at all.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut short: String = line.chars().take(200).collect();
    if line.chars().count() > 200 {
        short.push('…');
    }
    short
}

/// The answer shape a run is held to, and whether the caller said anything about it.
///
/// Three inputs, in one place, because the rule has a case for each and getting it wrong is silent
/// in the way that matters: a session which quietly stopped being held to its schema would go on
/// answering JSON while nobody checked it, and a resumed run which quietly *adopted* a new shape
/// from somewhere other than its file would have invented a contract.
///
/// `--schema` names a file or holds the schema itself when it starts with `{`. A path is read here,
/// where the failure can still say which file, rather than at argument-parsing time where the only
/// possible message is about the command line.
struct Shaping {
    schema: Option<schema::Schema>,
    /// The raw schema as the caller wrote it, for the session file: what a reader should see is the
    /// contract, not flint's parsed reading of it.
    schema_json: Option<serde_json::Value>,
    /// Whether the caller said something about the shape (`--schema` or `--no-schema`), which is
    /// what decides whether this run writes a `schema` line.
    stated: bool,
}

fn resolve_output_schema(
    args: &Args,
    session: Option<&session::LoadedSession>,
) -> Result<Shaping> {
    let stated = args.schema.is_some() || args.no_schema;
    let raw = match (&args.schema, args.no_schema) {
        // Both at once is a contradiction, and choosing an order for them would be inventing a rule
        // nobody asked for. Refused, with the reason.
        (Some(_), true) => {
            return usage(
                "--schema and --no-schema were both given: one says what shape the answer must \
                 take and the other says there is no shape. Use one.",
            )
        }
        (Some(value), false) => {
            let text = if value.trim_start().starts_with('{') {
                value.clone()
            } else {
                std::fs::read_to_string(value)
                    .with_context(|| format!("cannot read the schema file '{value}'"))
                    .map_err(|e| -> anyhow::Error { Usage(format!("{e:#}")).into() })?
            };
            let parsed = schema::Schema::parse(&text)
                .with_context(|| format!("cannot use the schema from '{value}'"))
                .map_err(|e| -> anyhow::Error { Usage(format!("{e:#}")).into() })?;
            return Ok(Shaping {
                schema_json: serde_json::from_str(&text).ok(),
                schema: Some(parsed),
                stated: true,
            });
        }
        // Inherited: what the conversation's own file says it was last held to, which is nothing at
        // all for a session that never had one.
        (None, false) => session.and_then(|loaded| loaded.output_schema.clone()),
        (None, true) => None,
    };
    let schema = match &raw {
        Some(value) => Some(schema::Schema::parse(&value.to_string()).with_context(|| {
            // The schema in the file was accepted when it was written, so this is the file being
            // edited by hand or written by a newer flint whose subset is larger. Either way the
            // answer is the same: say which conversation and which keyword.
            "the schema recorded in this session is not one this build can check"
        })?),
        None => None,
    };
    Ok(Shaping {
        schema,
        schema_json: raw,
        stated,
    })
}

/// What a page's controls can be drawn from, as the frame named `state`.
///
/// §8's read channel, and the reason it comes before any control: a picker cannot be built
/// without knowing the options, and the page must not read `config.toml` for them. A second
/// reader of the same state is a second thing that can disagree with the process -- the file may
/// say one thing while this run does another, which is exactly what `/config`'s "this run; the
/// file says" line exists to report -- so the process says what it knows and the page renders it.
///
/// Built from the values *in force* rather than from the file: the agent's guard rather than
/// `cfg.readonly`, and the printer's verbosity, which is three-valued while the file's `verbose`
/// is a bool. Nothing secret goes in it: names, models, and the settings that are enumerable.
fn state_frame(
    cfg: &config::Config,
    provider: &config::ProviderConfig,
    agent: &agent::Agent,
    printer: &Printer<'_>,
) -> String {
    // `choices` is what `/model` offers, and the same function fills the page's picker, so the
    // two cannot come to disagree about which model names are valid.
    let providers: Vec<serde_json::Value> = cfg
        .providers
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "models": p.choices(),
            })
        })
        .collect();
    serde_json::json!({
        "type": "state",
        "provider": provider.name,
        "model": provider.model,
        "toggles": toggles(agent, printer),
        "providers": providers,
        "commands": page_commands(cfg, provider, agent),
    })
    .to_string()
}

/// The page's menu: the commands it may offer, in the shape a control is drawn from.
///
/// `label` is what `/help` prints, so the page can show the command's own words; `send` is what
/// goes on the wire, so the page never assembles a command line out of parts; `class` is §8's
/// class, which is what a control is chosen by -- and what the drift test in `tests/cli_output.rs`
/// filters on to run only the commands that are safe to run with no argument.
///
/// Two kinds of row are left out rather than marked: the toggles, which the `toggles` field carries
/// in the shape a switch needs, and the terminal's own commands, which have no business on a page
/// (`/exit` above all: a misclick must not end a session).
fn page_commands(
    cfg: &config::Config,
    provider_cfg: &config::ProviderConfig,
    agent: &agent::Agent,
) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = page_rows(cfg, provider_cfg, agent)
        .into_iter()
        .map(|row| {
            let mut json = serde_json::json!({
                "label": row.label,
                "send": row.send,
                "help": row.help,
                "class": row.class,
            });
            // Omitted when there are none, which is every row but `/skills`: a frame that carried
            // `"values": []` on twenty-eight rows would be saying "no values" twenty-eight times.
            if !row.values.is_empty() {
                json["values"] = serde_json::json!(row.values);
            }
            // And the same for the answers the page may ask for. Each is carried as its own object
            // because the page needs three things about it -- what to call it, how to draw it, and
            // whether the command works without it -- and the word for how to draw it is the input's
            // `type`, so the page is told *how* to draw a credential rather than left to decide from
            // the command's name that a key is a secret.
            if !row.args.is_empty() {
                json["fields"] = serde_json::json!(row
                    .args
                    .iter()
                    .map(|arg| serde_json::json!({
                        "field": arg.field.word(),
                        "name": arg.name,
                        "optional": arg.optional,
                    }))
                    .collect::<Vec<_>>());
            }
            // And for a destructive row, which list its argument comes from. The page has to draw
            // the choices before anything can be confirmed, and the two lists it has are different.
            if let Some(from) = row.from {
                json["from"] = serde_json::json!(from.word());
            }
            json
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// One row of the page's menu, before it becomes JSON.
///
/// A struct rather than `serde_json::json!` straight away, because the *report route* has to ask the
/// same question the page asks -- "may I send this line?" -- and rebuilding that answer by parsing
/// the frame's own JSON would be a second implementation of the menu, which is the mistake this
/// whole file's single table exists to avoid. One list, rendered once and checked once.
struct PageRow {
    label: &'static str,
    send: &'static str,
    /// What the page says the row does: the table's own sentence, with the provider's name filled in
    /// where the table says "the active provider" (see `page_help`). A `String` for that one
    /// substitution, and for nothing else.
    help: String,
    class: &'static str,
    /// The argument values this page may ask for, when the row takes one it may *read*.
    ///
    /// Carried on the row rather than looked up in a `providers`-style field, because it is the
    /// permission as much as the options. On a `panel` row a value is a reading, and the report route
    /// admits it; on a `selector` row -- `/provider <name>`, `/model <name>` -- it is a list the page
    /// may *type* for the reader, and the route refuses it, because running a provider switch with the
    /// terminal quiet would start a local engine without printing a word anywhere.
    values: Vec<String>,
    /// The answers this row's line is made of, when the page may ask for them.
    ///
    /// The table's own list, in the table's own order, because what the page sends is
    /// `send` followed by the answers in that order: a page that reordered them, or dropped one and
    /// closed the gap, would be writing a different command.
    args: &'static [PageArg],
    /// On a destructive row: which list the page offers the argument from.
    from: Option<ArgFrom>,
}

/// Every row the page may offer, with the values it may be given.
///
/// Takes the config as well as the agent because three rows' options come from it: the providers this
/// run can switch to, the models the one in force offers, and the name in the key row's own sentence.
/// All three are the same facts the header's pickers and `/provider` are built from, so a page cannot
/// offer a provider the config does not have, or a model the active provider does not.
fn page_rows(
    cfg: &config::Config,
    provider_cfg: &config::ProviderConfig,
    agent: &agent::Agent,
) -> Vec<PageRow> {
    COMMANDS
        .iter()
        .filter_map(|row| {
            row.on_page.class().map(|class| PageRow {
                label: row.label,
                send: row.send,
                help: page_help(row, provider_cfg),
                class,
                // The rows whose argument this run has a list for. `/skills <name>` is a read, so the
                // page may ask for it, and the names are the ones this run's prompt was built with --
                // see `Agent::skills`, which is why a directory walk is not needed here. The two
                // switches take their options from the config, which is also where the pickers get
                // them: the reader is shown the names rather than asked to remember one.
                values: match (row.send, row.on_page) {
                    ("/skills", _) => agent.skills().to_vec(),
                    ("/provider", OnPage::Selector) => {
                        cfg.providers.iter().map(|p| p.name.clone()).collect()
                    }
                    ("/model", OnPage::Selector) => provider_cfg.choices(),
                    // `/resume <n|id>` is the third selector and has no values on purpose: its list is
                    // the sidebar's, where the numbers are and where a press already resumes.
                    _ => Vec::new(),
                },
                args: row.args,
                from: row.from,            })
        })
        .collect()
}

/// The sentence the page shows for a row: the table's own, with the one thing the page can say better.
///
/// `/provider key`'s help says "the active provider", which is a phrase somebody looking at a browser
/// cannot resolve -- and which provider the key is for is exactly the fact that reader needs. The
/// frame knows the name, so it substitutes it: "set the API key for stub". That is the whole of the
/// licence here, and `tests/cli_output.rs` holds the line -- a frame's help must be the table's
/// sentence, or the table's sentence with that name filled in, so the page can never grow a second
/// description of a command that drifts from `/help`.
fn page_help(row: &CommandHelp, provider_cfg: &config::ProviderConfig) -> String {
    row.help.replace("the active provider", &provider_cfg.name)
}

/// What the feed is told asked for a command's answer.
///
/// The line itself, except when the command's argument is a credential. `/provider key <key>` writes
/// a key into the config file, and a `command` frame goes to *every* page connected to this run and
/// stays in the event ring -- so echoing the line would hand a key back to the browser that just
/// typed it into a masked field, and to anything else watching. The row says which commands those
/// are (`Field::Password`), so a command added later is redacted by its own row rather than by a
/// list here that nobody would remember to update. The bare `send` is what the page gets instead:
/// its own block then reads `/provider key`, which is what the reader did.
fn echoed_input(line: &str) -> &str {
    for row in COMMANDS {
        // Any answer of the row, not just the first: a row that took a key as its second word would
        // be redacted by its own table entry rather than by a list here that nobody would remember.
        if row.args.iter().any(|arg| arg.field == Field::Password) && takes_argument(row.send, line) {
            return row.send;
        }
    }
    line
}

/// Whether `line` is this command *with* an argument after it, rather than a longer word that merely
/// begins with the same letters.
fn takes_argument(send: &str, line: &str) -> bool {
    line.strip_prefix(send)
        .is_some_and(|rest| rest.starts_with(' '))
}

impl OnPage {
    /// The word this class is carried as, or `None` for the two the frame does not offer.
    fn class(self) -> Option<&'static str> {
        match self {
            OnPage::Panel => Some("panel"),
            OnPage::Button => Some("button"),
            OnPage::Selector => Some("selector"),
            OnPage::Form => Some("form"),
            OnPage::Danger => Some("danger"),
            OnPage::Toggles | OnPage::Terminal => None,
        }
    }
}

/// The settings a page can switch, each with the values it takes and the value it is on.
///
/// The same shape for all of them, and the shape is the point: `name`, `values`, `value` is
/// everything a `<select>` needs and everything the command needs (`/<name> <value>`), so the
/// page carries no list of its own -- not the toggles, not the words they take, not which one is
/// on. A page with its own copy of that is a page that can offer a word the command refuses.
///
/// Built from the values *in force*: the printer's level, which is three-valued while the file's
/// key is a word, and the agent's guard rather than `cfg.readonly`. Nothing secret goes in it.
fn toggles(agent: &agent::Agent, printer: &Printer<'_>) -> serde_json::Value {
    /// The word for a two-valued toggle. `/detail` and `/readonly` both take `on` and `off`.
    fn on_off(on: bool) -> &'static str {
        if on {
            "on"
        } else {
            "off"
        }
    }

    let verbose = display::Verbosity::ALL.map(|level| level.word());
    serde_json::json!([
        {
            "name": "verbose",
            "values": verbose,
            "value": display::Verbosity::from_level(printer.verbosity()).word(),
        },
        { "name": "detail", "values": ["off", "on"], "value": on_off(printer.tool_detail()) },
        { "name": "readonly", "values": ["off", "on"], "value": on_off(agent.readonly()) },
    ])
}

/// Nothing is being waited for any more.
fn status_done(printer: &Printer<'_>, live: Option<&web::Live>) {
    printer.term().activity_done();
    sink::announce_status(printer, live, false);
}

/// Nothing is being waited for any more, and the turn is over.
///
/// The terminal clears its status row; a browser is told both facts -- the cleared status, and that
/// the turn has ended. The second half is not cosmetic. An answer that is still arriving is closed
/// by `message.completed`, which the agent emits when a step *finishes*, and an interrupted turn
/// never gets there: the future was dropped. So the page that pressed stop went on showing a half
/// answer as though it were still being written, which is the one thing a stop is supposed to end.
///
/// The line is the one `--json` ends a turn with, from the same function, because a second spelling
/// of "the turn is over" is a second thing to keep in step with the first.
fn turn_over(
    printer: &Printer<'_>,
    live: Option<&web::Live>,
    agent: &agent::Agent,
    outcome: ndjson::Outcome,
) {
    status_done(printer, live);
    if let Some(live) = live {
        live.line(ndjson::turn_completed(agent.last_usage(), outcome));
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
) -> Result<Handover> {
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = printer.pal;
    ensure_usable(provider_cfg)?;

    let mut current = input.to_string();

    // Tell the browser what was asked, before the answer starts arriving.
    //
    // `turn.started` is the line `--json` already writes for exactly this, and the page already
    // renders it as the user's turn. Until this existed the page learned about a message only by
    // re-reading the session file, so a question typed into the *terminal* was invisible to a
    // browser watching the same conversation -- and a question typed into the browser would have
    // been invisible to the page that sent it, which is the shape of bug a composer must not
    // have. Pushed here rather than at each call site because every turn passes through here,
    // including the one a steering line starts.
    if let Some(live) = live {
        live.line(ndjson::turn_started(&current));
    }

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
    //
    // From here the turn is rendered by `sink::EventSink`, which is the same rendering
    // `examples/live_turn.rs` replays: it was a closure body here and a hand copy there, and the copy
    // drifted twice, so a layout fault was chased in the wrong file.
    let mut sink = sink::EventSink::new(printer, live);
    sink.begin_round();

    // Reports the page asked for while this turn is running. They wait for the turn: a listing
    // printed into the middle of an answer would be read as part of the answer, and the model's own
    // output is not something to interleave a reading with. Declared out here because a steered turn
    // runs the loop again and has to carry them to the end either way.
    let mut reports: Vec<String> = Vec::new();

    loop {
        // A tool round is another wait: the step starts by asking the model again, and the clock has
        // to be running for it or the pause after every tool call looks like the turn is over. The
        // round's own state -- which tools are in flight, which are being waited for, the answer so
        // far -- is the sink's, and this is where it starts over.
        sink.begin_round();

        // The turn and its output closure are confined to this scope: the future holds a mutable
        // borrow of the sink, and that borrow has to end before the code below can read it to decide
        // what to flush.
        let (result, steering, hand_back) = {
            let mut turn = Box::pin(agent.run(&current, |event| sink.event(event)));

            // Whichever happens first: the model finishes this turn, or the user
            // says something. Dropping the future cancels the HTTP stream, which
            // is exactly what an interrupt should do.
            //
            // Polling rather than a channel select, for the same reason as
            // next_line: the stream is woken by the network, the input by the tick.
            let mut result = None;
            let mut steering = None;
            // A line the REPL has to deal with. Kept apart from `steering`, which is a line the
            // model is about to be asked: one of them becomes a prompt and the other must not.
            let mut hand_back = None;
            let mut interrupted = false;
            // The turn's future is polled once *before* the channel is read, and that ordering is the
            // whole of this fix. `Agent::run` pushes the user's message on its first poll, so a line
            // already waiting in the channel used to be taken first -- and taken as an interrupt, which
            // drops a future that had never run a single step: the question was in the history and in
            // the file nowhere, and the command went on as if it had been asked. The window is
            // milliseconds wide in a real session, which is why it was found by accident and left
            // alone; polling here makes the order a fact rather than a race, whatever a fast typist or
            // a browser does. The result is ignored on purpose: a turn that manages to finish inside
            // this one poll has already been dealt with by the loop below, and an interrupt's own error
            // is not news -- that is what `interrupted` means two lines further down.
            let _ = std::future::poll_fn(|cx| {
                let _ = std::future::Future::poll(turn.as_mut(), cx);
                std::task::Poll::Ready(())
            })
            .await;
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
                        // `/stop` is the interrupt as a word, and the word exists because a key is
                        // not always available: over ssh, in a piped session, or from the browser,
                        // where the composer can send a line and nothing else. It is deliberately
                        // *not* handed back to the REPL -- the line has done its work, and a command
                        // running after the thing it stopped would have only "nothing is running" to
                        // say, which reads as the stop having failed.
                        if matches!(line.trim(), "/stop") {
                            interrupted = true;
                        } else if for_the_repl(&line) {
                            hand_back = Some(line);
                        } else {
                            steering = Some(line);
                        }
                        break;
                    }
                    // A report is a read, not a person speaking, and that is why it falls through
                    // instead of breaking: a line here interrupts the turn because somebody typed
                    // it, and nobody typed this. It is kept for the REPL, which is the only place
                    // that knows what a command means, and answered once the model is done -- an
                    // answer that raced the turn would be a second writer in the transcript.
                    Ok(InputMsg::Report(asked)) => reports.push(asked),
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
                            sink::named(printer, live, NO_RESPONSE_LABEL);
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
            (result, steering, hand_back)
        };

        // Whatever this turn drew and never committed belongs to the conversation.
        //
        // An interrupt is a dropped future -- `/stop`, Ctrl-C, or the line that steers the
        // next prompt -- so the agent loop never reaches the code that turns a step's text
        // into a message, and the answer stays on screen with no record of it anywhere. That
        // is the reported fault, and it is the expensive kind: nothing fails, the user reads
        // half an answer and asks about it, and the model answers as if it had never been
        // written. Committed here, by the code that did the dropping, because the agent
        // cannot do it for itself after its future is gone.
        agent.commit_drawn_answer();
        // ...and it is not the answer *in flight* any more, for the same reason: it is in the file.
        // See `Live::answer_committed` -- a page opening after a stop would otherwise be handed the
        // stopped half as an answer still being written, because `message.completed` is what
        // normally drains that state and an interrupted turn never reaches it.
        if let Some(live) = live {
            live.answer_committed();
        }

        // The same three values as the `--json` path, decided the same way and from the same facts:
        // a dropped turn is a stopped one (that is what `None` means here), a turn that ran out of
        // steps is unfinished, and anything else finished. The page reads this, so a browser and a
        // program are told the same thing about the same run.
        let outcome = match (&result, agent.ran_out_of_steps()) {
            (None, _) => ndjson::Outcome::Stopped,
            (_, true) => ndjson::Outcome::Incomplete,
            _ => ndjson::Outcome::Complete,
        };

        // The line was not for the model, so the turn stops here rather than answering it. The
        // request in flight is dropped, which is what `Interrupt` does too -- a command is not a
        // reason to keep paying for an answer nobody is waiting for any more.
        if let Some(line) = hand_back {
            turn_over(printer, live, agent, outcome);
            return Ok(Handover { line: Some(line), reports });
        }

        // Whatever happened, nothing is running now: leaving a stale clock on the strip
        // would be worse than showing none.
        turn_over(printer, live, agent, outcome);

        match steering {
            None => {
                // The turn finished. `stream` already rendered the whole answer, so
                // there is no trailing fragment left to flush -- only the line
                // break that ends it.
                if sink.streamed() {
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
                if sink.streamed() {
                    // A partial answer is on screen and is no longer valid.
                    printer.term().blank();
                }
                printer.interrupted();
                printer.term().line(format_args!("{bold}> {reset}{}", printer.dim(&text)));
                if let Some(live) = live {
                    live.line(ndjson::turn_started(&text));
                }
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
    // Nothing handed back: everything the turn was given, it used. Reports can still be here -- the
    // page can ask for one at any moment, including the last moment of a turn.
    Ok(Handover { line: None, reports })
}

/// What a turn left for the REPL.
///
/// Two things, and neither is part of the conversation: a line the turn could not use, and the
/// reports the page asked for while it was working. They are separate fields rather than one queue
/// because they are handled differently and in this order -- the line is what a person typed and
/// goes first, and a report is a read that waits until the person has been answered.
#[derive(Default)]
struct Handover {
    line: Option<String>,
    reports: Vec<String>,
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
/// model", and a constant for it would be a second way to say the same thing. The two labels
/// a *stream* can be in -- `thinking` and `writing the answer` -- are `sink`'s, because they are
/// decided inside the rendering both callers share.
/// How long silence may last before it stops looking like a model thinking.
const NO_RESPONSE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
const NO_RESPONSE_LABEL: &str = "no response yet — the network or the endpoint may be stuck";

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
            // `--fork` names a session the same way `--resume` does, and the difference is what
            // happens to the file rather than to the conversation: the original is left alone and
            // the run writes the copy. Bare, it forks the most recent, the same tolerance `--resume`
            // has, because "take the last conversation and branch it" is the whole use.
            "--fork" => {
                let next = iter.peek().cloned().unwrap_or_default();
                if next.is_empty() || next.starts_with('-') || next == "exec" {
                    args.fork = Some(String::new());
                } else {
                    iter.next();
                    args.fork = Some(next);
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
            "--schema" => {
                args.schema = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--schema requires a value"))?,
                )
            }
            "--no-schema" => args.no_schema = true,
            "exec" => {
                let rest: Vec<String> = iter.by_ref().collect();
                if rest.is_empty() {
                    return Err(anyhow!("exec requires a command"));
                }
                args.exec = Some(rest.join(" "));
            }
            "balance" => {
                // A command rather than a flag: it is a question with an answer, and it may well want
                // arguments of its own later. The word is taken before the bare-word fallback below,
                // which would otherwise turn `flint balance` into a prompt asking the model to guess.
                args.balance = true;
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
  flint --continue                 resume the most recent session in this directory
  flint --resume <n|id>            resume a particular session
  flint --fork [<n|id>]            copy a session and continue the copy, leaving the original alone
  flint exec <command>             run a command directly (no model, no network)
  flint balance [--json]           ask the provider whether it can be used, and what is left
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
                      (/web opens the same thing from inside a conversation)
  --port <n>          the port for --web (default 0: any free one)
  --cwd <dir>         working directory for tools
  --schema <file|json>  with -p --json: require the answer to match this schema, and
                      report it as a `result` line once it does. A path, or `{{...}}`
                      for the schema itself. Recorded in the session, so --resume
                      holds the conversation to the same shape without repeating it
  --no-schema         answer in prose even if this session's file says otherwise
  --no-color          disable ANSI colour (also honours NO_COLOR)
  -h, --help          this message

{b}LOCAL ENGINES{r}
  A local model server is started and stopped as you switch providers.
    start = \"...\"     run when flint needs this provider and nothing answers at its URL
    stop  = \"...\"     run when a switch leaves it behind
    start = \"\"        not flint's to manage — leave it alone
  Leaving `start` out uses what flint knows: a provider named `ollama`, `mlx` or `llamacpp`
  gets its command from the name, when the endpoint is local, the program is on PATH and the
  model is named. Anything else is said plainly rather than attempted.
  Engine output: <FLINT_HOME>/engines/<provider>.log

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

/// Which exit code a failed run deserves, from the cause when flint knows it.
///
/// A table in one place rather than a chain of conditions at the call site: this is the part a caller
/// branches on, so it should be readable and testable by itself. The two numbers that carry weight:
///
/// - **69 `EX_UNAVAILABLE`**: nothing about this question changes the answer. No key, credentials the
///   provider rejected, an account with nothing in it. A person has to act.
/// - **75 `EX_TEMPFAIL`**: the retries already ran out, so asking again later is exactly right. The
///   convention has a code for that, and it was declared and unused until the provider started saying
///   which failures are worth retrying.
///
/// `1` means "flint does not know", which is the honest answer rather than a cause that has not been
/// established.
fn exit_code_for(failure: Option<&provider::ProviderFailure>, provider_was_unusable: bool) -> i32 {
    let Some(failure) = failure else {
        // The provider was already known not to be configured when the run began, so this failure is
        // that fault arriving late rather than a new one.
        return if provider_was_unusable {
            EXIT_UNAVAILABLE
        } else {
            EXIT_FAILURE
        };
    };
    if failure.retryable {
        return EXIT_TEMPFAIL;
    }
    match failure.code {
        "insufficient_balance" | "auth" | "no_key" => EXIT_UNAVAILABLE,
        _ => EXIT_FAILURE,
    }
}

/// One line about a provider, for a person: what was checked, and what it said.
///
/// Three answers, not two, and the third is the point: "cannot tell" is what an endpoint that serves
/// only `/chat/completions` gives, and printing it as usable would be a claim nothing checked. The
/// check that produced the answer is named for the same reason -- "usable" from a `/models` probe is a
/// different statement from "usable" from a balance endpoint, and only one of them is about money.
fn human_balance(name: &str, balance: &provider::Balance) -> String {
    let money = match (&balance.total, &balance.currency) {
        (Some(total), Some(currency)) => format!("{total} {currency}"),
        (Some(total), None) => total.clone(),
        _ => String::new(),
    };
    let parts = match (&balance.granted, &balance.topped_up) {
        (Some(granted), Some(topped)) => format!(" ({granted} granted, {topped} topped up)"),
        _ => String::new(),
    };
    match balance.usable {
        Some(true) if money.is_empty() => format!(
            "{name}: usable -- /models answered. This provider publishes no balance, so the account \
             was not asked about"
        ),
        Some(true) => format!("{name}: usable -- {money} left{parts}"),
        Some(false) if money.is_empty() => format!(
            "{name}: not usable -- the provider will not serve requests as things are (no balance, or \
             credentials it rejects)"
        ),
        Some(false) => format!("{name}: not usable -- {money} left{parts}"),
        None => format!(
            "{name}: cannot tell -- the endpoint answered neither its balance nor /models, so neither \
             the key nor the account was checked. A provider that serves only /chat/completions is \
             normal here"
        ),
    }
}

/// The preflight: ask the provider about itself, print the answer, and pick the exit code from the same
/// table a failed run uses.
///
/// The codes are the vocabulary already shipped rather than a private one: `0` usable, `69` a person
/// must act (no key, rejected credentials, an empty account), `75` the check could not get out and is
/// worth repeating, `1` reached but not decidable. A batch can therefore branch on `flint balance`
/// exactly as it branches on a run that failed -- which is the point of asking before spending.
async fn balance_and_stop(cfg: config::ProviderConfig, json: bool) -> Result<i32> {
    let name = cfg.name.clone();
    let provider = provider::Provider::new(cfg)?;
    match provider.balance().await {
        Ok(balance) => {
            let code = match balance.usable {
                Some(true) => EXIT_OK,
                Some(false) => EXIT_UNAVAILABLE,
                None => EXIT_FAILURE,
            };
            if json {
                let mut object = serde_json::Map::new();
                object.insert("type".to_string(), serde_json::json!("balance"));
                object.insert("provider".to_string(), serde_json::json!(name));
                object.insert("checked".to_string(), serde_json::json!(balance.checked));
                // `null` rather than absent for the verdict: the field is always there to read, and
                // `null` is exactly "no verdict", which is what an absent field cannot say.
                object.insert("usable".to_string(), serde_json::json!(balance.usable));
                // The figures are absent when the provider did not publish them -- a wrong balance is
                // worse than no balance, and a caller can tell "not asked" from "asked, and it said
                // nothing" by reading `checked`.
                for (field, value) in [
                    ("currency", &balance.currency),
                    ("total_balance", &balance.total),
                    ("granted_balance", &balance.granted),
                    ("topped_up_balance", &balance.topped_up),
                ] {
                    if let Some(value) = value {
                        object.insert(field.to_string(), serde_json::json!(value));
                    }
                }
                println!("{}", serde_json::Value::Object(object));
            } else {
                println!("{}", human_balance(&name, &balance));
            }
            Ok(code)
        }
        Err(failure) => {
            if json {
                println!(
                    "{}",
                    ndjson::error_coded(&failure.message, failure.code, failure.retryable)
                );
            } else {
                eprintln!("flint: {}", failure.message);
            }
            Ok(exit_code_for(Some(&failure), false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The browser line is a command *line*, so the URL is quoted inside it and handed over
    /// verbatim rather than passed as an ordinary argument.
    ///
    /// The form this replaced -- `["/C", "start", "", url]` -- was measured cutting a URL at
    /// `&`, eating a caret, and failing outright when the target had a space in it: see the
    /// table on `browser_invocation`. What is asserted here is the shape, because the
    /// behaviour was measured with a `.cmd` standing in for the browser and running the real
    /// line would open a window.
    #[test]
    fn the_browser_line_is_a_quoted_command_line() {
        let url = "http://127.0.0.1:3080/?a=1&b=2";
        let run = browser_invocation(url);
        if cfg!(target_os = "windows") {
            assert_eq!(run.program, "cmd");
            assert_eq!(run.args, vec!["/S".to_string(), "/C".to_string()]);
            let raw = run.raw.expect("cmd needs the line verbatim");
            assert_eq!(raw, format!("\"start \"\" \"{url}\"\""));
            assert!(
                raw.contains(&format!("\"{url}\"")),
                "the URL must be inside quotes of its own, or `&` ends the command: {raw}"
            );
            assert!(
                raw.starts_with("\"start \"\" "),
                "the empty quotes are the window title, not decoration: {raw}"
            );
        } else {
            assert_eq!(run.raw, None, "only cmd needs the verbatim form");
            if cfg!(target_os = "macos") {
                assert_eq!(run.program, "open");
            } else {
                assert_eq!(run.program, "xdg-open");
            }
            assert_eq!(run.args, vec![url.to_string()]);
        }
    }

    /// The code a caller branches on comes from the cause, not from the fact that something failed.
    ///
    /// Written as a table because it *is* one: each row is a decision about what a program should do
    /// next, and the two rows that were worth a code of their own are the ones where the wrong advice
    /// is expensive -- an empty account retried for ever, or a temporary fault abandoned.
    #[test]
    fn the_cause_decides_the_exit_code() {
        let failure = |code: &'static str, retryable: bool| provider::ProviderFailure {
            code,
            retryable,
            message: format!("a {code} failure"),
        };

        // A person has to do something: add funds, fix the key.
        for code in ["insufficient_balance", "auth", "no_key"] {
            assert_eq!(
                exit_code_for(Some(&failure(code, false)), false),
                EXIT_UNAVAILABLE,
                "{code} is not a generic failure"
            );
        }
        // The retries already ran out, so trying again later is the right advice.
        for code in ["rate_limit", "server", "network"] {
            assert_eq!(
                exit_code_for(Some(&failure(code, true)), false),
                EXIT_TEMPFAIL,
                "{code} is worth another try, and the code has to say so"
            );
        }
        // Retryable outranks the name: a 5xx that said "insufficient_quota" is still the quota.
        assert_eq!(
            exit_code_for(Some(&failure("insufficient_balance", true)), false),
            EXIT_TEMPFAIL,
            "the retryable flag is the provider's answer, not a hint"
        );
        // Nothing established a cause: say so rather than name one.
        assert_eq!(exit_code_for(Some(&failure("unknown", false)), false), EXIT_FAILURE);
        assert_eq!(exit_code_for(None, false), EXIT_FAILURE);
        assert_eq!(
            exit_code_for(None, true),
            EXIT_UNAVAILABLE,
            "a provider known to be unconfigured is the same fault arriving late"
        );
    }

    /// A line that was already waiting does not erase the question it interrupts.    ///
    /// The hole ROADMAP §9 measured by accident and deliberately left alone: a line already in the
    /// channel when a turn starts used to be read *before the turn's future was ever polled*, and the
    /// turn's user message is pushed by that first poll -- so a `/model x` quick enough to beat it (or
    /// sent from the browser, which has no keyboard to blame) switched away from a question that had
    /// never been written down, in the history or in the file. Nothing failed and nothing said so.
    ///
    /// The window is milliseconds wide in a real session, which is why the fix is not a slower version
    /// of the same race: the turn is polled once *before* the channel is read, so the ordering no
    /// longer depends on time. That is also what makes this test possible -- the line goes into the
    /// channel before `run_turn` is called, which is exactly the state the old order got wrong and
    /// which no amount of waiting can tell apart from a fast typist.
    #[tokio::test]
    async fn a_line_that_was_already_waiting_does_not_erase_the_question() {
        // A listener that accepts and then says nothing, so the turn stays in flight and the queued
        // line has to be what ends it. The same shape §9's own measurement used.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to sit on");
        let port = listener.local_addr().expect("the port").port();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let _ = std::io::Read::read(&mut socket, &mut [0u8; 1024]);
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });

        let cfg = config::Config {
            default_provider: "stub".to_string(),
            providers: vec![config::ProviderConfig {
                name: "stub".to_string(),
                base_url: format!("http://127.0.0.1:{port}/v1"),
                api_key: "test".to_string(),
                model: "stub-model".to_string(),
                models: vec!["stub-model".to_string(), "stub-other".to_string()],
                api_key_env: None,
                start: None,
                stop: None,
                start_timeout_secs: 0,
                proxy: None,
            }],
            ..config::Config::default()
        };
        let provider_cfg = cfg.providers[0].clone();

        let term = Term::plain();
        let printer = Printer::new(false, display::Verbosity::Off.level(), &term);
        let provider = provider::Provider::new(provider_cfg.clone()).expect("a provider");
        let mut agent = agent::Agent::new(
            &cfg,
            provider,
            false,
            std::env::temp_dir(),
            None,
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(InputMsg::Line("/model stub-other".to_string()))
            .expect("the line is in the channel before the turn starts");

        let handover = run_turn(
            &mut agent,
            &provider_cfg,
            "the question",
            &printer,
            &mut rx,
            true,
            None,
        )
        .await;

        let asked = agent.history().iter().any(|message| {
            matches!(message, event::Message::User { content } if content.contains("the question"))
        });
        assert!(
            asked,
            "the question is in neither the history nor the file: the queued line was taken before \
             the turn's own first poll, so the turn never existed. History: {:?}",
            agent.history()
        );
        let handed = handover.expect("the turn left a line for the REPL");
        assert_eq!(
            handed.line.as_deref(),
            Some("/model stub-other"),
            "the queued command was not handed back to the REPL, so it was executed as a steer"
        );
    }
}
