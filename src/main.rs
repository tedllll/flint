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

use flint::{agent, config, event, provider, session, tools, util};

use anyhow::{anyhow, Context, Result};
use event::Event;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

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

    let code = match runtime.block_on(real_main()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("\x1b[31mflint: error:\x1b[0m {e:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn real_main() -> Result<i32> {
    let args = parse_args(std::env::args().skip(1).collect())?;

    if args.help {
        print_help();
        return Ok(0);
    }

    if args.no_color {
        // Handled by the Printer below; nothing global to set.
    }

    let mut cfg = config::Config::load()?;
    let color = !args.no_color && std::env::var_os("NO_COLOR").is_none();
    let cwd = match &args.cwd {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().context("cannot determine working directory")?,
    };

    // ---- exec: no model, no network. Pure command passthrough. ----
    if let Some(command) = &args.exec {
        return exec_direct(&cfg, command, &cwd, color).await;
    }

    // ---- list sessions ----
    if args.list_sessions {
        let sessions = session::list(&config::sessions_dir())?;
        if sessions.is_empty() {
            println!("(no sessions yet)");
        }
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
    if key.trim().is_empty() && !is_local_endpoint(&provider_cfg.base_url) {
        return Err(anyhow!(
            "no API key for provider '{}'.\n\
             Either put it in {} (field `api_key`), or export {} in your shell.",
            provider_cfg.name,
            config::config_path().display(),
            provider_cfg
                .api_key_env
                .as_deref()
                .unwrap_or("YOUR_API_KEY_ENV")
        ));
    }

    // ---- resume a session, if asked ----
    let mut history: Vec<event::Message> = Vec::new();
    let mut resumed_id: Option<String> = None;
    if args.continue_last {
        match session::latest(&config::sessions_dir())? {
            Some(path) => {
                let loaded = session::load(&path)?;
                history = loaded.messages;
                resumed_id = Some(loaded.id.clone());
                if !loaded.model.is_empty() && args.model.is_none() {
                    provider_cfg.model = loaded.model.clone();
                }
            }
            None => eprintln!("flint: no previous session found; starting a new one."),
        }
    }

    let provider = provider::Provider::new(provider_cfg.clone())?;
    let writer = if resumed_id.is_some() {
        None // appending to a resumed session file is not supported; keep it read-only
    } else {
        Some(session::SessionWriter::create(
            &config::sessions_dir(),
            &cwd,
            &provider_cfg.name,
            &provider_cfg.model,
        )?)
    };

    let mut agent = agent::Agent::new(&cfg, provider, readonly, cwd.clone(), writer);
    if !history.is_empty() {
        // Drop the fresh system prompt, splice in the loaded conversation.
        let mut merged = vec![event::Message::system(agent::SYSTEM_PROMPT)];
        merged.extend(
            history
                .into_iter()
                .filter(|m| !matches!(m, event::Message::System { .. })),
        );
        *agent.history_mut() = merged;
    }

    let printer = Printer { color };

    // ---- one-shot ----
    if let Some(prompt) = args.prompt {
        run_turn(&mut agent, &prompt, &printer).await?;
        println!();
        return Ok(0);
    }

    // ---- interactive ----
    interactive(&mut cfg, &mut agent, &provider_cfg, &printer).await?;
    Ok(0)
}

/// The REPL. Slash commands carry every convenience feature; the main loop
/// stays deliberately tiny.
async fn interactive(
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    printer: &Printer,
) -> Result<()> {
    println!(
        "{BOLD}flint{RESET} {DIM}v{}{RESET}  {}",
        env!("CARGO_PKG_VERSION"),
        printer.style(DIM, "last spark in the dark")
    );
    println!(
        "  provider {BOLD}{}{RESET}   model {BOLD}{}{RESET}   cwd {DIM}{}{RESET}",
        provider_cfg.name,
        provider_cfg.model,
        agent.cwd().display()
    );
    if agent.readonly() {
        println!(
            "  {}",
            printer.style(GREEN, "READONLY — writes and mutating commands are refused")
        );
    }
    println!("  {DIM}/help for commands, /exit to quit, !cmd to run a shell command{RESET}");
    println!();

    let stdin = std::io::stdin();
    loop {
        print!("{BOLD}>{RESET} ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        let n = stdin.lock().read_line(&mut line).unwrap_or(0);
        if n == 0 {
            println!();
            break; // EOF (Ctrl-D / piped input exhausted)
        }
        let input = line.trim_end_matches(['\r', '\n']).trim().to_string();
        if input.is_empty() {
            continue;
        }

        if let Some(rest) = input.strip_prefix('!') {
            run_shell_escape(cfg, rest, agent.cwd(), printer).await;
            continue;
        }

        if input.starts_with('/') {
            match handle_command(&input, cfg, agent, provider_cfg, printer).await? {
                Flow::Continue => continue,
                Flow::Exit => break,
                Flow::NewAgent(new_agent) => {
                    *agent = new_agent;
                    continue;
                }
            }
        }

        match run_turn(agent, &input, printer).await {
            Ok(()) => {}
            Err(e) => {
                println!("\n{} {e:#}", printer.style(RED, "error:"));
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

async fn handle_command(
    input: &str,
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    printer: &Printer,
) -> Result<Flow> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "/exit" | "/quit" | "/q" => return Ok(Flow::Exit),

        "/help" | "/?" => {
            println!(
                "\
{DIM}commands{RESET}
  /help                 this message
  /exit                 quit
  /provider [name]      list providers, or switch to one
  /model [name]         show or change the model
  /usage                context and token accounting
  /readonly [on|off]    toggle the write guard
  /tools                list available tools
  /sessions             list past sessions
  /new                  start a fresh conversation
  !<command>            run a shell command without the model
{DIM}notes{RESET}
  Permission model is full by default. /readonly is the only guard.
  Config file: {RESET}{}",
                config::config_path().display()
            );
        }

        "/provider" => {
            if arg.is_empty() {
                println!("{DIM}providers:{RESET}");
                for p in &cfg.providers {
                    let mark = if p.name == provider_cfg.name {
                        "*"
                    } else {
                        " "
                    };
                    let key_state =
                        if p.resolved_key().trim().is_empty() && !is_local_endpoint(&p.base_url) {
                            printer.style(RED, " (no key)")
                        } else {
                            String::new()
                        };
                    println!(
                        "  {mark} {:<12} {:<34} {}{key_state}",
                        p.name, p.base_url, p.model
                    );
                }
                println!("{DIM}switch with /provider <name>{RESET}");
            } else {
                let target = cfg
                    .provider(arg)
                    .ok_or_else(|| anyhow!("unknown provider '{arg}'"))?
                    .clone();
                let provider = provider::Provider::new(target.clone())?;
                let writer = Some(session::SessionWriter::create(
                    &config::sessions_dir(),
                    agent.cwd(),
                    &target.name,
                    &target.model,
                )?);
                let new_agent =
                    agent::Agent::new(cfg, provider, agent.readonly(), agent.cwd().clone(), writer);
                println!(
                    "switched to {BOLD}{}{RESET} ({})",
                    target.name, target.model
                );
                return Ok(Flow::NewAgent(new_agent));
            }
        }

        "/model" => {
            if arg.is_empty() {
                println!("model: {BOLD}{}{RESET}", provider_cfg.model);
            } else {
                println!(
                    "{DIM}hint: set `model` in {} to make this permanent.{RESET}",
                    config::config_path().display()
                );
            }
        }

        "/usage" => match agent.last_usage() {
            Some(u) => {
                println!(
                    "last request:  prompt {BOLD}{}{RESET}  completion {BOLD}{}{RESET}  total {BOLD}{}{RESET} tokens",
                    u.prompt_tokens,
                    u.completion_tokens,
                    u.total()
                );
                println!(
                    "{DIM}prompt tokens = your current context size. Nothing is trimmed automatically; \
                     use /new if it grows too large.{RESET}"
                );
            }
            None => println!("{DIM}no usage reported yet by this provider{RESET}"),
        },

        "/readonly" => {
            let turn_on = match arg {
                "on" => true,
                "off" => false,
                "" => !agent.readonly(),
                other => return Err(anyhow!("expected on|off, got '{other}'")),
            };
            if turn_on {
                println!(
                    "{}",
                    printer.style(GREEN, "readonly ON — no writes, no mutating commands")
                );
            } else {
                println!("{}", printer.style(RED, "readonly OFF — full permissions"));
            }
            println!(
                "{DIM}note: takes effect on the next /new or restart (the tool set is per agent).{RESET}"
            );
        }

        "/tools" => {
            println!("{DIM}tools:{RESET} {}", agent.tool_names().join(", "));
        }

        "/sessions" => {
            let sessions = session::list(&config::sessions_dir())?;
            if sessions.is_empty() {
                println!("(no sessions yet)");
            }
            for (id, summary) in sessions {
                println!("  {id}  {summary}");
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
            println!("started a new session");
            return Ok(Flow::NewAgent(new_agent));
        }

        other => {
            println!("{DIM}unknown command '{other}'. /help for the list.{RESET}");
        }
    }
    Ok(Flow::Continue)
}

/// Execute one user turn, streaming output to the terminal.
async fn run_turn(agent: &mut agent::Agent, input: &str, printer: &Printer) -> Result<()> {
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut streamed_text = false;
    let mut buffer = String::new();

    let result = agent
        .run(input, |event| match event {
            Event::Text(t) => {
                buffer.push_str(&t);
                // Flush complete lines as they arrive so output streams.
                while let Some(pos) = buffer.find('\n') {
                    let line: String = buffer.drain(..=pos).collect();
                    print!("{line}");
                }
                if !buffer.is_empty() {
                    print!("{buffer}");
                    buffer.clear();
                }
                std::io::stdout().flush().ok();
                streamed_text = true;
            }
            Event::Reasoning(t) => {
                print!("{}", printer.style(DIM, &t));
                std::io::stdout().flush().ok();
            }
            Event::ToolStart { id, name } => {
                printer.tool_start(&name);
                tool_names.insert(id, name);
            }
            Event::ToolArgs { id, args } => {
                let name = tool_names.get(&id).cloned().unwrap_or_default();
                printer.tool_args(&name, &args);
            }
            Event::ToolResult { output, ok, .. } => {
                printer.tool_result(&output, ok);
            }
            Event::Usage(_) => {}
            Event::Warning(w) => {
                println!("\n{} {w}", printer.style(RED, "warning:"));
            }
            Event::Done => {}
        })
        .await;

    if !buffer.is_empty() {
        print!("{buffer}");
    }
    if streamed_text {
        println!();
    }
    result?;

    if let Some(u) = agent.last_usage() {
        println!(
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
        );
    }
    Ok(())
}

/// `!cmd` escape inside the REPL and the `exec` subcommand.
async fn run_shell_escape(
    cfg: &config::Config,
    command: &str,
    cwd: &std::path::Path,
    _printer: &Printer,
) {
    if command.trim().is_empty() {
        println!("usage: !<command>");
        return;
    }
    match tools::run_command_raw(cfg, command, cwd, 600).await {
        Ok(out) => print!("{out}"),
        Err(e) => println!("{RED}error:{RESET} {e:#}"),
    }
}

async fn exec_direct(
    cfg: &config::Config,
    command: &str,
    cwd: &std::path::Path,
    _color: bool,
) -> Result<i32> {
    // A direct exec is meant for real work (installs, rebuilds), so it gets a
    // generous ceiling rather than the conversational default.
    let output = tools::run_command_raw(cfg, command, cwd, 1800).await?;
    print!("{output}");
    Ok(0)
}

fn is_local_endpoint(base_url: &str) -> bool {
    let u = base_url.to_ascii_lowercase();
    u.contains("localhost")
        || u.contains("127.0.0.1")
        || u.contains("0.0.0.0")
        || u.contains("[::1]")
}

struct Printer {
    color: bool,
}

impl Printer {
    fn style(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    fn tool_start(&self, name: &str) {
        println!(
            "\n{} {}",
            self.style(CYAN, "[tool]"),
            self.style(BOLD, name)
        );
    }

    fn tool_args(&self, name: &str, args: &str) {
        if args.trim().is_empty() || args.trim() == "{}" {
            return;
        }
        let display = if name == "bash" {
            serde_json::from_str::<serde_json::Value>(args)
                .ok()
                .and_then(|v| {
                    v.get("command")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| util::json_preview(args, 200))
        } else {
            util::json_preview(args, 200)
        };
        println!("       {}", self.style(DIM, &display));
    }

    fn tool_result(&self, output: &str, ok: bool) {
        let label = if ok {
            self.style(GREEN, "[ok]")
        } else {
            self.style(RED, "[fail]")
        };
        let lines: Vec<&str> = output.lines().collect();
        let shown = 12.min(lines.len());
        println!("       {label}");
        for line in lines.iter().take(shown) {
            println!("       {}", self.style(DIM, line));
        }
        if lines.len() > shown {
            println!(
                "       {}",
                self.style(
                    DIM,
                    &format!(
                        "... {} more lines (full output was sent to the model)",
                        lines.len() - shown
                    )
                )
            );
        }
    }
}

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

fn print_help() {
    println!(
        "\
{BOLD}flint{RESET} — a minimal cross-platform rescue agent

{BOLD}USAGE{RESET}
  flint                            interactive session
  flint -p \"<prompt>\"              one-shot, prints the answer and exits
  flint <words...>                 same as -p
  flint --continue                 resume the most recent session
  flint exec <command>             run a command directly (no model, no network)
  flint --list-sessions            show saved sessions

{BOLD}OPTIONS{RESET}
  --provider <name>   use a specific provider          (config: default_provider)
  --model <name>      override the model for this run
  --readonly          refuse writes and mutating commands
  --cwd <dir>         working directory for tools
  --no-color          disable ANSI colour (also honours NO_COLOR)
  -h, --help          this message

{BOLD}CONFIG{RESET}
  {}

{BOLD}WHY{RESET}
  This exists so that when your usual tooling breaks, you still have something
  that can talk to a model and run commands to repair it. It is deliberately
  small, dependency-light and hand-editable.",
        config::config_path().display()
    );
}
