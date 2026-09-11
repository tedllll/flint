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
const YELLOW: &str = "\x1b[33m";
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

    // Decide colour before anything prints. Honours NO_COLOR as well as the flag.
    let color = !args.no_color && std::env::var_os("NO_COLOR").is_none();

    if args.help {
        print_help(color);
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
    if let Some(command) = &args.exec {
        let cfg = config::Config::load().unwrap_or_default();
        return exec_direct(&cfg, command, &cwd, color).await;
    }

    // Everything below may need a provider, so a config is required from here.
    let mut cfg = config::Config::load()?;

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
    // A missing key is NOT fatal here. The whole point of the in-tool config is
    // that a fresh machine can start `flint`, run /provider add, and become
    // usable -- refusing to start would lock the user out of the very command
    // that fixes the problem. The check happens when a message is actually sent.
    let key_missing = key.trim().is_empty() && !is_local_endpoint(&provider_cfg.base_url);

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
        run_turn(&mut agent, &provider_cfg, &prompt, &printer).await?;
        println!();
        return Ok(0);
    }

    // ---- interactive ----
    interactive(&mut cfg, &mut agent, &provider_cfg, &printer, key_missing).await?;
    Ok(0)
}

/// The REPL. Slash commands carry every convenience feature; the main loop
/// stays deliberately tiny.
async fn interactive(
    cfg: &mut config::Config,
    agent: &mut agent::Agent,
    provider_cfg: &config::ProviderConfig,
    printer: &Printer,
    key_missing: bool,
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
    if key_missing {
        // Say what to do, and say it where the user is already looking. This is
        // the first thing a fresh machine sees, so it has to be actionable
        // without leaving the tool.
        println!(
            "  {}",
            printer.style(
                YELLOW,
                "no API key for this provider — the shell tools still work."
            )
        );
        println!(
            "  {DIM}set one with {RESET}{BOLD}/provider key <key>{RESET}{DIM}, or configure a \
             different provider with {RESET}{BOLD}/provider add{RESET}"
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

        match run_turn(agent, provider_cfg, &input, printer).await {
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

/// Prompt on stdout and read one line from stdin.
///
/// Everything here is plain line input: no raw mode, no terminal library, no
/// cursor control. That is deliberate -- the configuration wizard has to work
/// over SSH, in a dumb pipe, and on a machine where the terminal is one of the
/// things that is broken.
fn prompt(label: &str, current: Option<&str>, _color: bool) -> Result<String> {
    let shown = match current {
        Some(c) if !c.is_empty() => format!(" {DIM}[{c}]{RESET}"),
        _ => String::new(),
    };
    print!("  {label}{shown}: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line)?;
    if n == 0 {
        return Err(anyhow!("input ended"));
    }
    let v = line.trim().to_string();
    Ok(if v.is_empty() {
        current.unwrap_or("").to_string()
    } else {
        v
    })
}

/// Ask a yes/no question. Empty answer takes `default`.
fn confirm(label: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    print!("  {label} [{hint}]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(default);
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Ok(default),
    }
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
    printer: &Printer,
) -> Result<Flow> {
    let existing = editing.and_then(|n| cfg.provider(n)).cloned();
    let verb = if existing.is_some() {
        "editing"
    } else {
        "new provider"
    };
    println!("\n{BOLD}{verb}{RESET} {DIM}(blank = keep current value){RESET}");

    let name = prompt("name", existing.as_ref().map(|p| p.name.as_str()), true)?;
    if name.trim().is_empty() {
        return Err(anyhow!("a provider needs a name"));
    }

    let base_url = prompt(
        "base_url",
        existing
            .as_ref()
            .map(|p| p.base_url.as_str())
            .or(Some("https://api.deepseek.com/v1")),
        true,
    )?;
    let model = prompt(
        "model",
        existing
            .as_ref()
            .map(|p| p.model.as_str())
            .or(Some("deepseek-chat")),
        true,
    )?;

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
    println!("{DIM}  (the key is stored in plain text in the config file; you can also use an env var instead){RESET}");
    let api_key = prompt(
        "api_key (or leave blank)",
        if key_hint.is_empty() {
            None
        } else {
            Some(key_hint.as_str())
        },
        true,
    )?;
    // If the user accepted the hint, keep the existing key rather than writing
    // the literal "<set, N chars>" into the config.
    let api_key = if api_key == key_hint {
        current_key.to_string()
    } else {
        api_key
    };

    let env_name = prompt(
        "api_key_env (read key from this env var; wins over api_key)",
        if current_env.is_empty() {
            None
        } else {
            Some(current_env.as_str())
        },
        true,
    )?;

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
    println!(
        "{} {BOLD}{name}{RESET} {} {}",
        printer.style(GREEN, "saved"),
        if added { "added to" } else { "updated in" },
        config::config_path().display()
    );
    if !had_key && !is_local {
        println!(
            "{}",
            printer.style(
                YELLOW,
                "  no key yet — set one with /provider key, or the request will fail"
            )
        );
    }

    if confirm(&format!("switch to '{name}' now?"), true)? {
        cfg.default_provider = name.clone();
        cfg.save()?;
        return switch_provider(cfg, &name, agent.cwd(), agent.readonly());
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
) -> Result<Flow> {
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
    println!(
        "switched to {BOLD}{}{RESET} ({})",
        target.name, target.model
    );
    Ok(Flow::NewAgent(new_agent))
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
  /provider             list providers
  /provider <name>      switch to one
  /provider add         set up a new provider (interactive)
  /provider edit <name> change one (interactive)
  /provider key <key>   set the API key for the active provider
  /provider rm <name>   delete one
  /config [edit]        show or change shell, steps, proxy
  /model [name]         show or change the model
  /usage                context and token accounting
  /readonly [on|off]    toggle the write guard
  /tools                list available tools
  /sessions             list past sessions
  /new                  start a fresh conversation
  !<command>            run a shell command without the model
{DIM}notes{RESET}
  Permission model is full by default. /readonly is the only guard.
  Everything above is configurable from inside flint; the file is only there
  so that it stays hand-editable when that is easier.
  Config file: {RESET}{}",
                config::config_path().display()
            );
        }

        "/provider" => {
            // Subcommands come first: a provider literally called "add" is a lot
            // less likely than someone wanting to add one.
            let mut sub = arg.splitn(2, char::is_whitespace);
            let first = sub.next().unwrap_or("");
            let rest = sub.next().unwrap_or("").trim();

            match first {
                "" => {
                    println!("{DIM}providers:{RESET}");
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
                        println!(
                            "  {mark} {:<12} {:<34} {}{key_state}",
                            p.name, p.base_url, p.model
                        );
                    }
                    println!(
                        "{DIM}  /provider <name>       switch\n  \
                         /provider add          configure a new one\n  \
                         /provider edit <name>  change one\n  \
                         /provider key <key>    set the key for the active one\n  \
                         /provider rm <name>    delete one{RESET}"
                    );
                }

                "add" => return provider_wizard(cfg, agent, None, printer).await,

                "edit" => {
                    if rest.is_empty() {
                        return Err(anyhow!("usage: /provider edit <name>"));
                    }
                    if cfg.provider(rest).is_none() {
                        return Err(anyhow!("unknown provider '{rest}'"));
                    }
                    return provider_wizard(cfg, agent, Some(rest), printer).await;
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
                    println!(
                        "{} removed {BOLD}{rest}{RESET}; default is now {BOLD}{fallback}{RESET}",
                        printer.style(GREEN, "ok")
                    );
                    if provider_cfg.name == rest {
                        return switch_provider(cfg, &fallback, agent.cwd(), agent.readonly());
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
                    println!(
                        "{} key saved for {BOLD}{}{RESET} ({})",
                        printer.style(GREEN, "ok"),
                        target.name,
                        config::config_path().display()
                    );
                    return switch_provider(cfg, &target.name, agent.cwd(), agent.readonly());
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
                    println!(
                        "switched to {BOLD}{}{RESET} ({})",
                        target.name, target.model
                    );
                    return Ok(Flow::NewAgent(new_agent));
                }
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

        "/config" => {
            println!("{DIM}config: {}{RESET}", config::config_path().display());
            println!("  default_provider = {BOLD}{}{RESET}", cfg.default_provider);
            println!(
                "  shell            = {BOLD}{}{RESET} {:?}",
                cfg.shell, cfg.shell_args
            );
            println!("  max_steps        = {}", cfg.max_steps);
            println!(
                "  proxy            = {}",
                cfg.proxy.as_deref().unwrap_or("(none)")
            );

            if arg == "edit" {
                // A small wizard, so the settings that matter when you are
                // stuck are reachable without hand-editing TOML.
                println!("\n{BOLD}settings{RESET} {DIM}(blank = keep){RESET}");
                let shell = prompt("shell", Some(&cfg.shell), printer.color)?;
                let args = prompt(
                    "shell_args (space separated)",
                    Some(&cfg.shell_args.join(" ")),
                    printer.color,
                )?;
                let steps = prompt("max_steps", Some(&cfg.max_steps.to_string()), printer.color)?;
                let proxy = prompt(
                    "proxy (e.g. http://127.0.0.1:10808, blank to clear)",
                    cfg.proxy.as_deref(),
                    printer.color,
                )?;

                cfg.shell = shell;
                cfg.shell_args = args.split_whitespace().map(str::to_string).collect();
                cfg.max_steps = steps.parse().unwrap_or(cfg.max_steps);
                cfg.proxy = if proxy.trim().is_empty() {
                    None
                } else {
                    Some(proxy)
                };
                cfg.save()?;
                println!(
                    "{} saved to {}",
                    printer.style(GREEN, "ok"),
                    config::config_path().display()
                );
            } else {
                println!("{DIM}  /config edit   change shell, steps, proxy{RESET}");
                println!("{DIM}  (provider settings: /provider){RESET}");
            }
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
    printer: &Printer,
) -> Result<()> {
    ensure_usable(provider_cfg)?;
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
    let outcome = tools::run_command_detailed(cfg, command, cwd, 1800).await?;
    print!("{}", outcome.report);
    // Propagate the child's status. `flint exec` is meant to be usable from
    // scripts, so a failing command must make flint itself fail -- reporting
    // success here would make the exit code meaningless.
    Ok(outcome.code)
}

fn is_local_endpoint(base_url: &str) -> bool {
    provider::is_local_endpoint(base_url)
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

fn print_help(color: bool) {
    // The escape sequences are applied only when colour is wanted, so
    // `flint --no-color --help` is plain text (and piping stays clean).
    let (b, r) = if color { (BOLD, RESET) } else { ("", "") };
    println!(
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
    );
}
