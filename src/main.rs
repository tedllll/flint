//! flint — a minimal cross-platform rescue agent.
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

use flint::{agent, config, display, event, provider, session, term, tools};

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
    provider: Option<String>,
    model: Option<String>,
    readonly: bool,
    no_color: bool,
    list_sessions: bool,
    exec: Option<String>,
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
        let cfg = config::Config::load().unwrap_or_default();
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
        // one session per line.
        for (id, summary) in sessions {
            println!("{id}  {summary}");
        }
        return Ok(0);
    }

    if cfg.readonly != args.readonly {
        // --readonly can only turn it on for this run; the config default wins.
    }
    let readonly = cfg.readonly || args.readonly;

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
    if args.continue_last {
        match session::latest(&config::sessions_dir())? {
            Some(path) => {
                let loaded = session::load(&path)?;
                history = loaded.messages;
                if !loaded.model.is_empty() && args.model.is_none() {
                    provider_cfg.model = loaded.model.clone();
                }
                eprintln!(
                    "flint: resumed {} ({} messages)",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    history.len()
                );
                resumed = Some(path);
            }
            None => eprintln!("flint: no previous session found; starting a new one."),
        }
    }

    let provider = provider::Provider::new(provider_cfg.clone())?;
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

    let mut agent = agent::Agent::new(&cfg, provider, readonly, cwd.clone(), writer);
    if !history.is_empty() {
        // Drop the fresh system prompt, splice in the loaded conversation, and
        // put a freshly built prompt back -- it carries run-time facts (the
        // shell dialect, the working directory) that a stored transcript cannot
        // be trusted to still be accurate about.
        let mut merged = vec![event::Message::system(agent::build_system_prompt(
            &cfg, &cwd,
        ))];
        merged.extend(
            history
                .into_iter()
                .filter(|m| !matches!(m, event::Message::System { .. })),
        );
        *agent.history_mut() = merged;
    }

    // The terminal comes first: it decides whether there is an input row to keep
    // clear, and the printer needs it for every line it emits.
    //
    // One-shot mode has no input row to reserve -- the caller wants the text and
    // nothing else -- so it leaves the terminal alone.
    let interactive_mode = args.prompt.is_none();
    let term = std::sync::Arc::new(if interactive_mode {
        term::Term::start()?
    } else {
        term::Term::plain()
    });

    let (reader, mut input_rx) = if term.interactive() {
        InputReader::from_terminal(std::sync::Arc::clone(&term))
    } else {
        InputReader::from_stdin()
    };

    let printer = Printer::new(
        color,
        if cfg.verbose { CHATTY } else { NORMAL },
        &term,
    );

    // ---- one-shot ----
    if let Some(prompt) = args.prompt {
        run_turn(&mut agent, &provider_cfg, &prompt, &printer, &mut input_rx).await?;
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
    /// The user asked to quit (Ctrl-C on an empty line, Ctrl-D, or EOF).
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
async fn interactive(
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &mut config::ProviderConfig,
    printer: &Printer<'_>,
    key_missing: bool,
    reader: &InputReader,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputMsg>,
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
                Flow::NewAgent(new_agent) => {
                    *agent = new_agent;
                    continue;
                }
            }
        }

        match run_turn(agent, provider_cfg, &input, printer, input_rx).await {
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
    NewAgent(agent::Agent),
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
        model,
        api_key_env: if env_name.trim().is_empty() {
            None
        } else {
            Some(env_name)
        },
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
    Ok(Flow::NewAgent(new_agent))
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
  /verbose [on|off|full] how much tool detail to print
  /readonly [on|off]    toggle the write guard
  /tools                list available tools
  /sessions             list past sessions
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
                    return Ok(Flow::NewAgent(new_agent));
                }
            }
        }

        "/model" => {
            if arg.is_empty() {
                printer.term().line(format_args!("model: {bold}{}{reset}", provider_cfg.model));
            } else {
                printer.term().line(format_args!(
                    "{dim}hint: set `model` in {} to make this permanent.{reset}",
                    config::config_path().display()
                ));
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
                _ => "full — arguments and more output",
            };
            printer.term().line(format_args!("{} {what}", printer.style(GREEN, "verbose")));
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

        "/sessions" => {
            let sessions = session::list(&config::sessions_dir())?;
            if sessions.is_empty() {
                printer.term().line(format_args!("(no sessions yet)"));
            }
            for (id, summary) in sessions {
                printer.term().line(format_args!("  {id}  {summary}"));
            }
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
            return Ok(Flow::NewAgent(new_agent));
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
            return Ok(Flow::NewAgent(new_agent));
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

async fn run_turn(
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    input: &str,
    printer: &Printer<'_>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<InputMsg>,
) -> Result<()> {
    #[allow(unused_variables)]
    let Palette { dim, bold, red, green, cyan, yellow, reset } = printer.pal;
    ensure_usable(provider_cfg)?;

    let mut current = input.to_string();

    loop {
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut streamed_text = false;
        // The answer so far. Streamed output is redrawn in full on every fragment,
        // because a fragment is not a line: it can stop in the middle of a word,
        // and only the whole text can be placed correctly.
        let mut answer = String::new();

        // The turn and its output closure are confined to this scope: the future
        // holds a mutable borrow of `buffer`, and that borrow has to end before
        // the code below can read `buffer` to decide what to flush.
        let (result, steering) = {
            let mut turn = Box::pin(agent.run(&current, |event| match event {
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
                Event::Reasoning(t) => {
                    // The model thinking out loud. Worth watching when something
                    // is going wrong, noise the rest of the time.
                    if printer.verbosity() >= CHATTY {
                        printer.term().stream(&printer.style(DIM, &t));
                        std::io::stdout().flush().ok();
                    }
                }
                Event::ToolStart { id, name } => {
                    tool_names.insert(id, name);
                }
                Event::ToolArgs { id, args } => {
                    let name = tool_names.get(&id).cloned().unwrap_or_default();
                    printer.tool_call(&name, &args);
                }
                Event::ToolResult { output, ok, .. } => {
                    printer.tool_result(&output, ok);
                }
                Event::Usage(_) => {}
                Event::Warning(w) => {
                    printer.term().blank();
                    printer.term().line(format_args!("{} {w}", printer.style(RED, "warning:")));
                }
                Event::Done => {}
            }));

            // Whichever happens first: the model finishes this turn, or the user
            // says something. Dropping the future cancels the HTTP stream, which
            // is exactly what an interrupt should do.
            //
            // Polling rather than a channel select, for the same reason as
            // next_line: the stream is woken by the network, the input by the tick.
            let mut result = None;
            let mut steering = None;
            loop {
                // Only a submitted line interrupts. `Quit` here means stdin ended
                // (a one-shot run, or a script that closed the pipe) -- treating
                // it as steering would abort the turn before it ever started,
                // which is exactly what `flint -p ...  | cat` used to do.
                if let Ok(InputMsg::Line(line)) = input_rx.try_recv() {
                    steering = Some(line);
                    break;
                }
                tokio::select! {
                    r = &mut turn => {
                        result = Some(r);
                        break;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(2)) => {}
                }
            }
            (result, steering)
        };

        match steering {
            None => {
                // The turn finished. `stream` already rendered the whole answer, so
                // there is no trailing fragment left to flush -- only the line
                // break that ends it.
                if streamed_text {
                    printer.term().blank();
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

/// Reduce a tool's JSON arguments to the one value worth showing on a line.
fn parse_args(argv: Vec<String>) -> Result<Args> {
    let mut args = Args::default();
    let mut iter = argv.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => args.help = true,
            "-p" | "--prompt" => {
                args.prompt = Some(
                    iter.next()
                        .ok_or_else(|| anyhow!("--prompt requires a value"))?,
                )
            }
            "-c" | "--continue" | "--resume" => args.continue_last = true,
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
            "--list-sessions" => args.list_sessions = true,
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
    Ok(args)
}

fn print_help(color: bool, term: &Term) {
    // The escape sequences are applied only when colour is wanted, so
    // `flint --no-color --help` is plain text (and piping stays clean).
    let (b, r) = if color { (BOLD, RESET) } else { ("", "") };
    term.line(format_args!(
        "\
{b}flint{r} — a minimal cross-platform rescue agent

{b}USAGE{r}
  flint                            interactive session
  flint -p \"<prompt>\"              one-shot, prints the answer and exits
  flint <words...>                 same as -p
  flint --continue                 resume the most recent session
  flint exec <command>             run a command directly (no model, no network)
  flint --list-sessions            show saved sessions

{b}OPTIONS{r}
  --provider <name>   use a specific provider          (config: default_provider)
  --model <name>      override the model for this run
  --readonly          refuse writes and mutating commands
  --cwd <dir>         working directory for tools
  --no-color          disable ANSI colour (also honours NO_COLOR)
  -h, --help          this message

{b}CONFIG{r}
  {}

{b}WHY{r}
  This exists so that when your usual tooling breaks, you still have something
  that can talk to a model and run commands to repair it. It is deliberately
  small, dependency-light and hand-editable.",
        config::config_path().display()
    ));
}
