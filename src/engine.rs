//! Bringing a local model engine up, and letting it go.
//!
//! flint's provider list is mostly about *endpoints*, and most endpoints are somebody else's
//! process: an API on the internet that is either reachable or not. A local one is different
//! in three ways that matter. It has to be running before the endpoint answers at all. It
//! holds several gigabytes of memory while it is. And nothing else on the machine is going to
//! start it — the launch script is a line in a README.
//!
//! So a provider may name a command that starts it and a command that stops it, and flint runs
//! them when it needs the provider and when it leaves it. Measured on the machine this was
//! written for: three local runtimes (ollama, llama.cpp, MLX), a 24 GB machine that cannot
//! hold two of the models at once, and a `/provider` switch that until now produced a
//! connection error and a memory bill.
//!
//! # What flint does not do
//!
//! **It does not own the process**, and that is a decision rather than an omission. The two
//! launch scripts on that machine behave in opposite ways: one `exec`s the server, so the
//! script *becomes* it and waiting on the command would wait forever; the other `nohup`s it
//! and exits, so a child handle would be a handle on nothing. Owning the child would work for
//! the first and silently do nothing for the second — the worst kind of difference, because
//! the failure looks like success. The configured commands are the interface, and what they
//! do is their own business.
//!
//! **It only ever runs what the config names.** There is no path here from a tool call: the
//! model cannot make flint start a program, and the `start` and `stop` strings come from the
//! user's own file, at the same level of trust as the `shell` setting.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::config::{Config, ProviderConfig};

/// How long to wait for an endpoint after starting it, when the config does not say.
///
/// Long, because this is loading billions of parameters off a disk: measured on the machine
/// above, a 12B model at Q5 took about fifteen seconds warm and the same model through a cold
/// page cache takes considerably longer.
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(180);

/// How long one stop command may take.
///
/// Short, because a stop command is a signal and a wait, not a build. A command that hangs
/// must not hang the switch that asked for it.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How often to ask whether it is up yet.
const POLL_EVERY: Duration = Duration::from_millis(500);

/// What flint would run for a provider, and where that decision came from.
///
/// Four outcomes rather than two, because "there is no command" and "there is no command and
/// that is worth explaining" are different things to the person reading the screen.
#[derive(Debug)]
enum Plan {
    /// The config names the commands, which always wins.
    Configured { start: String, stop: Option<String> },
    /// flint knows this engine by name, and the program it needs is reachable.
    Recognised { start: String, stop: Option<String> },
    /// flint knows the name but cannot run what that name implies, and the reason is worth
    /// saying: the user asked for this by naming the provider that way.
    CannotStart { why: String },
    /// Nothing here is a local engine flint manages.
    Nothing,
}

/// What flint knows about an engine whose name is also its program.
struct Known {
    /// The program the start command needs. Checked before the command is used: a built-in
    /// default that names a program this machine does not have is worse than no default,
    /// because the failure arrives later and looks like flint being broken.
    needs: &'static str,
    start: String,
    stop: Option<String>,
}

/// Work out what to run, without running anything.
///
/// The three states of `start` mean three different things, and the difference is the whole
/// interface:
///
/// * **absent** -- use what flint knows about this engine, if anything;
/// * **a command** -- use it, and stop guessing;
/// * **empty** -- do not manage this one. A provider that is somebody else's process, or one
///   started by hand, needs a way to say so, and an empty string is the way that needs no
///   new concept.
fn plan(provider: &ProviderConfig) -> Plan {
    match provider.start.as_deref() {
        Some(text) if text.trim().is_empty() => return Plan::Nothing,
        Some(command) => {
            return Plan::Configured {
                start: command.trim().to_string(),
                stop: named_command(provider.stop.as_deref()).map(str::to_string),
            }
        }
        None => {}
    }

    let Some(known) = known_engine(provider) else {
        return Plan::Nothing;
    };
    if !crate::tools::command_exists(known.needs) {
        return Plan::CannotStart {
            why: format!(
                "'{}' is a local engine flint knows how to start, but `{}` is not on PATH, \
                 so it cannot. Set `start` and `stop` for this provider — its own launch \
                 script is usually the right answer — or set `start = \"\"` to say flint \
                 should leave it alone.",
                provider.name, known.needs
            ),
        };
    }
    Plan::Recognised {
        start: known.start,
        stop: known.stop,
    }
}

/// The engines flint recognises by name, and only where the name is enough.
///
/// Kept to the three whose behaviour is well understood and whose invocation is a single
/// line. The machine this was written on is also the argument for the `needs` check above:
/// its `ollama` is not on PATH at all, and its ollama needs `OLLAMA_MODELS` pointed at a
/// directory outside the default or it starts with no models — so a hardcoded `ollama serve`
/// would have failed twice, in ways that look like flint being broken rather than like a
/// missing path.
fn known_engine(provider: &ProviderConfig) -> Option<Known> {
    // Only for an endpoint on this machine. A provider named `mlx` that points at another
    // host is a label on somebody else's server, and starting a local process for it is the
    // one way this could be actively wrong rather than merely unhelpful.
    if !crate::provider::is_local_endpoint(&provider.base_url) {
        return None;
    }
    let port = port_of(&provider.base_url)?;
    let name = provider.name.trim().to_ascii_lowercase();
    let model = provider.model.trim();

    match name.as_str() {
        // `ollama serve` takes its host and port from the environment, so the endpoint's
        // port is not part of the command.
        "ollama" => Some(Known {
            needs: "ollama",
            start: "ollama serve".to_string(),
            stop: Some(stop_for("ollama", "ollama serve")),
        }),
        "mlx" | "mlx-lm" | "mlxlm" if !model.is_empty() => Some(Known {
            needs: "mlx_lm.server",
            // `--model` is the path this server was started with: it treats an unknown name
            // as a HuggingFace repository to download, so the provider's `model` field has to
            // be that path anyway.
            start: format!(
                "mlx_lm.server --host 127.0.0.1 --port {port} --model {}",
                shell_word(model)
            ),
            stop: Some(stop_for("mlx_lm.server", "mlx_lm.server")),
        }),
        "llamacpp" | "llama-server" | "llamacpp-server" if !model.is_empty() => Some(Known {
            needs: "llama-server",
            start: format!(
                "llama-server -m {} --host 127.0.0.1 --port {port}",
                shell_word(model)
            ),
            stop: Some(stop_for("llama-server", "llama-server")),
        }),
        _ => None,
    }
}

/// The port an endpoint is on, when it says.
fn port_of(base_url: &str) -> Option<u16> {
    reqwest::Url::parse(base_url).ok()?.port_or_known_default()
}

/// Quote a value for the shell if it needs it.
///
/// The two shells disagree about which quote character quotes: `'` is an ordinary character to
/// `cmd`, so a single-quoted Windows path is a path with a quote in it. A Windows file name
/// cannot contain `"` at all, which is what makes the double quote total here — the value this
/// is ever handed is a model path.
fn shell_word(text: &str) -> String {
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:+".contains(c)) {
        return text.to_string();
    }
    if cfg!(windows) {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// A short description of what flint would do about a provider's engine, for `/provider`.
///
/// The point of showing it is that everything else about this is invisible: a
/// name-derived command never appears in the config, and behaviour nobody can see is the
/// thing this repository keeps arguing against. A person who cannot see that flint is about
/// to run `ollama serve` finds out only when it is wrong.
///
/// Empty when there is nothing to say, which is most providers.
pub fn describe(provider: &ProviderConfig) -> &'static str {
    match plan(provider) {
        Plan::Configured { .. } => "engine: configured",
        Plan::Recognised { .. } => "engine: from its name",
        Plan::CannotStart { .. } => "engine: from its name, but it cannot run here",
        Plan::Nothing => "",
    }
}

/// What flint did about an engine.
#[derive(Debug, PartialEq)]
pub enum Woke {
    /// The provider names no start command: running it is not flint's business.
    Unmanaged,
    /// Something was already answering, so nothing was run.
    AlreadyUp,
    /// The start command was run and the endpoint came up.
    Started { waited: Duration },
}

/// Whether something is answering at this endpoint.
///
/// `/models` because it is the one route every OpenAI-compatible server has and none of them
/// charge for, and because answering it is exactly the thing being asked about: is there
/// something listening that can take a request. Any failure counts as no.
pub async fn is_up(base_url: &str) -> bool {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(PROBE_TIMEOUT)
        .build()
    else {
        return false;
    };
    matches!(client.get(&url).send().await, Ok(response) if response.status().is_success())
}

/// Make sure the provider's endpoint is answering, starting it if the config says how.
///
/// `say` is called before the wait rather than after it, because a model that takes a minute
/// to load with nothing on screen is indistinguishable from a flint that has hung.
pub async fn ensure_up(
    provider: &ProviderConfig,
    config: &Config,
    mut say: impl FnMut(String),
) -> Result<Woke> {
    // Decided before anything is contacted, so a provider with nothing to start costs
    // nothing: probing an API on the internet to ask whether it is up is a request nobody
    // asked for.
    let (command, stop) = match plan(provider) {
        Plan::Nothing => return Ok(Woke::Unmanaged),
        Plan::CannotStart { why } => {
            // Said only when it matters. If the engine is already answering, the fact that
            // flint could not have started it is not news.
            if is_up(&provider.base_url).await {
                return Ok(Woke::AlreadyUp);
            }
            say(why);
            return Ok(Woke::Unmanaged);
        }
        Plan::Configured { start, stop } | Plan::Recognised { start, stop } => (start, stop),
    };
    let command = command.as_str();

    if is_up(&provider.base_url).await {
        return Ok(Woke::AlreadyUp);
    }
    let _ = &stop;

    let log = log_path(&provider.name)?;
    say(format!(
        "starting {} — `{command}`\n  output: {}",
        provider.name,
        log.display()
    ));
    spawn(command, config, &log)?;

    let timeout = start_timeout(provider);
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        tokio::time::sleep(POLL_EVERY).await;
        if is_up(&provider.base_url).await {
            let waited = started.elapsed();
            say(format!("{} is up after {:.0}s", provider.name, waited.as_secs_f32()));
            return Ok(Woke::Started { waited });
        }
    }

    Err(anyhow!(
        "'{}' did not answer at {} within {}s of running `{command}`.\n\
         Its output is in {} — that is where the reason will be.",
        provider.name,
        provider.base_url,
        timeout.as_secs(),
        log.display()
    ))
}

/// Run the provider's stop command, if it has one. Returns whether one ran.
pub async fn shut_down(
    provider: &ProviderConfig,
    config: &Config,
    mut say: impl FnMut(String),
) -> Result<bool> {
    // The same plan the start came from, so a provider flint started is a provider flint can
    // stop: an engine recognised by name supplies both halves or neither.
    let command = match plan(provider) {
        Plan::Configured { stop, .. } | Plan::Recognised { stop, .. } => stop,
        Plan::CannotStart { .. } | Plan::Nothing => None,
    };
    let Some(command) = command else {
        return Ok(false);
    };
    // Said before it happens, because for a recognised engine this is a `pkill -f` and the
    // pattern is the whole of what decides what dies. A user who has two servers of the same
    // kind running should see that before it runs, not after.
    say(format!("stopping {} — `{command}`", provider.name));
    let shell = crate::tools::probe_shell(&config.shell, &config.shell_args);
    let (program, args) = shell.split_first().expect("probe_shell always names a program");
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.arg(&command);

    // Waited on, unlike `start`: a stop command is a signal and a wait, and what happens
    // after it is the next provider's business. Bounded all the same, because a command that
    // never returns must not take the switch down with it.
    match tokio::time::timeout(STOP_TIMEOUT, cmd.status()).await {
        Ok(Ok(status)) if status.success() => Ok(true),
        Ok(Ok(status)) => Err(anyhow!(
            "the stop command for '{}' exited with {status}: `{command}`",
            provider.name
        )),
        Ok(Err(e)) => Err(anyhow!(
            "cannot run the stop command for '{}': {e}",
            provider.name
        )),
        Err(_) => Err(anyhow!(
            "the stop command for '{}' did not finish within {}s: `{command}`",
            provider.name,
            STOP_TIMEOUT.as_secs()
        )),
    }
}

/// Start it in the background, through a shell, and return as soon as the shell has forked.
///
/// The output goes to a file rather than to flint's terminal: the engine keeps printing after
/// flint has moved on, and a background process writing to a terminal that is being repainted
/// would tear the layout apart. When a start fails, this file is the only place the reason
/// exists.
///
/// That file is handed over as an open handle rather than named in the command with
/// `>> file 2>&1`, and the engine is detached by not waiting rather than with `&`. Both of
/// those are shell *syntax*, and flint's two shells do not agree about either one: `cmd` has no
/// `&` at all, and it reads a single-quoted path as part of the name, so `>> 'C:\...\log'` is
/// not a redirection to it but a file that cannot exist. Handing over a handle removes the
/// second reader instead of escaping for it.
///
/// Backgrounded rather than held as a child for the reason in the module comment: the two
/// natural ways to write a launcher need opposite treatment, and never waiting is the one form
/// that satisfies both. The engine ends up owned by the system rather than by flint, which is
/// what it should be — it outlives any one conversation, and a long-lived flint is not a
/// service manager.
fn spawn(command: &str, config: &Config, log: &Path) -> Result<()> {
    let shell = crate::tools::probe_shell(&config.shell, &config.shell_args);
    let (program, args) = shell.split_first().expect("probe_shell always names a program");

    // Opened before the spawn, so that a log flint cannot write is an error about the log
    // rather than a start that failed with nowhere to say why. Appended, because an engine's
    // output from the run before this one is the context for this one.
    let open = |path: &Path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("cannot write the engine log {}", path.display()))
    };
    let out = open(log)?;
    let err = out
        .try_clone()
        .with_context(|| format!("cannot write the engine log {}", log.display()))?;

    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .arg(command)
        // A server has no business reading flint's keyboard: it would take keystrokes meant
        // for the prompt. On Windows it matters twice, because a child that inherits the
        // console shares it with whatever is drawing on it.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::from(err));

    let child = cmd
        .spawn()
        .with_context(|| format!("cannot run `{command}`"))?;

    // Never waited on, but reaped: a launcher that `nohup`s and exits leaves a zombie behind
    // for as long as flint lives, and one thread per engine start is cheaper than a process
    // table slowly filling with them.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// `Some(command)` when a field names one, `None` when it is absent or blank.
fn named_command(field: Option<&str>) -> Option<&str> {
    field.map(str::trim).filter(|text| !text.is_empty())
}

fn start_timeout(provider: &ProviderConfig) -> Duration {
    if provider.start_timeout_secs == 0 {
        DEFAULT_START_TIMEOUT
    } else {
        Duration::from_secs(provider.start_timeout_secs)
    }
}

/// Where an engine's output goes: `<FLINT_HOME>/engines/<provider>.log`.
///
/// Under flint's own directory for the same reason everything else is: it is plain text, it
/// is where a person would look, and there is exactly one place to look.
fn log_path(provider: &str) -> Result<PathBuf> {
    let dir = crate::config::config_dir().join("engines");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir.join(format!("{}.log", safe_name(provider))))
}

/// A provider name as something safe to put in a file name.
fn safe_name(provider: &str) -> String {
    provider
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// The command that stops a recognised engine.
///
/// Neither shell has the other's way to say "the process running this". `pkill -f` matches
/// against the command line, and Windows has no `pkill`; `taskkill /IM` matches against the
/// image name instead, which for these engines is the same program spelled the way Windows
/// spells it. Both are as broad as their pattern: the switch says the command before it runs
/// it, which is what makes a broad match something the user can see and object to.
fn stop_for(image: &str, pattern: &str) -> String {
    if cfg!(windows) {
        format!("taskkill /IM {image}.exe /F")
    } else {
        format!("pkill -f '{pattern}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;

    fn provider(name: &str, base_url: &str, model: &str) -> ProviderConfig {
        ProviderConfig {
            name: name.to_string(),
            base_url: base_url.to_string(),
            api_key: "x".to_string(),
            model: model.to_string(),
            models: Vec::new(),
            api_key_env: None,
            start: None,
            stop: None,
            start_timeout_secs: 0,
            proxy: None,
        }
    }

    fn local(name: &str) -> ProviderConfig {
        provider(name, "http://127.0.0.1:8080/v1", "gemma")
    }

    /// The config always wins, which is what makes the built-in defaults safe to have.
    #[test]
    fn a_configured_command_is_used_exactly_as_written() {
        let mut p = local("ollama");
        p.start = Some("bash ~/gemma4-12b/ollama-start.sh".to_string());
        p.stop = Some("bash ~/gemma4-12b/ollama-stop.sh".to_string());
        match plan(&p) {
            Plan::Configured { start, stop } => {
                assert_eq!(start, "bash ~/gemma4-12b/ollama-start.sh");
                assert_eq!(stop.as_deref(), Some("bash ~/gemma4-12b/ollama-stop.sh"));
            }
            _ => panic!("a command in the config must be the one used"),
        }
    }

    /// An empty command is a statement, not an omission.
    #[test]
    fn an_empty_command_means_leave_this_one_alone() {
        let mut p = local("ollama");
        p.start = Some(String::new());
        assert!(
            matches!(plan(&p), Plan::Nothing),
            "an explicitly empty start must opt out of the built-in default"
        );
        p.start = Some("   ".to_string());
        assert!(matches!(plan(&p), Plan::Nothing), "whitespace is empty too");
    }

    /// A name flint knows, on this machine, where the program exists: flint can do something.
    ///
    /// Skipped where the program is absent rather than asserted around it, because the point
    /// of the test is the plan, not what happens to be installed on the machine running it.
    #[test]
    fn a_recognised_engine_gets_a_start_command_from_its_name() {
        if !crate::tools::command_exists("ollama") {
            return;
        }
        match plan(&local("ollama")) {
            Plan::Recognised { start, stop } => {
                assert_eq!(start, "ollama serve");
                assert!(stop.is_some(), "an engine flint starts, flint can stop");
            }
            other => panic!("an ollama provider on loopback should be recognised: {other:?}"),
        }
    }

    /// The case this machine is actually in, and the reason the `needs` check exists.
    ///
    /// A provider named `ollama` whose `ollama` is not on PATH must not produce a command
    /// that fails later, looking like flint is broken. It must say what is missing and how
    /// to fix it — the user asked for this by naming the provider that way.
    #[test]
    fn a_recognised_engine_that_cannot_be_run_says_why() {
        // A name flint knows, on an endpoint it does not.
        let remote = provider("ollama", "https://api.example.com/v1", "x");
        assert!(
            matches!(plan(&remote), Plan::Nothing),
            "a provider named ollama that points elsewhere is a label, not a local engine"
        );

        if crate::tools::command_exists("ollama") {
            return; // On a machine that has it, there is nothing to refuse.
        }
        match plan(&local("ollama")) {
            Plan::CannotStart { why } => {
                assert!(why.contains("ollama"), "it must name the program: {why}");
                assert!(why.contains("not on PATH"), "and where it looked: {why}");
                assert!(why.contains("start"), "and what to do about it: {why}");
            }
            other => panic!("a name flint knows with no program to run must explain: {other:?}"),
        }
    }

    /// Only the three engines whose name is enough, and only with a model to start.
    #[test]
    fn the_recognised_names_are_deliberately_few() {
        assert!(matches!(plan(&local("something-else")), Plan::Nothing));
        assert!(matches!(plan(&local("deepseek")), Plan::Nothing));
        // A name that needs a model to start, with no model configured, is not a plan:
        // there would be nothing to run the engine *on*.
        let modelless = |name: &str| provider(name, "http://127.0.0.1:8080/v1", "");
        assert!(matches!(plan(&modelless("mlx")), Plan::Nothing));
        assert!(matches!(plan(&modelless("llamacpp")), Plan::Nothing));
        // And a cloud endpoint is never a local engine, whatever it is called.
        assert!(matches!(
            plan(&provider("mlx", "https://api.deepseek.com/v1", "/a/model")),
            Plan::Nothing
        ));
    }

    #[test]
    fn a_recognised_command_carries_the_endpoint_it_serves() {
        let p = provider("mlx", "http://127.0.0.1:8081/v1", "/Users/me/models/Ornith-1.5");
        match plan(&p) {
            Plan::Recognised { start, .. } if crate::tools::command_exists("mlx_lm.server") => {
                assert!(start.contains("--port 8081"), "the endpoint's port: {start}");
                assert!(start.contains("/Users/me/models/Ornith-1.5"), "and its model: {start}");
            }
            Plan::CannotStart { .. } => {} // not installed here; covered by the test above
            other => panic!("unexpected plan: {other:?}"),
        }
    }

    /// A path with a space has to survive being put in a command.
    ///
    /// The expected string is written per platform because that *is* the assertion: the point
    /// is not that a space gets quoted but that it is quoted the way the shell in use reads it.
    #[test]
    fn a_model_path_is_quoted_when_it_needs_to_be() {
        assert_eq!(shell_word("/Users/me/model"), "/Users/me/model");
        if cfg!(windows) {
            assert_eq!(shell_word(r"C:\models\my model.gguf"), r#""C:\models\my model.gguf""#);
            assert_eq!(shell_word("it's"), "\"it's\"");
        } else {
            assert_eq!(shell_word("/Users/me/my model"), "'/Users/me/my model'");
            assert_eq!(shell_word("it's"), r#"'it'\''s'"#);
        }
    }

    #[test]
    fn a_port_is_read_from_the_endpoint() {
        assert_eq!(port_of("http://127.0.0.1:8080/v1"), Some(8080));
        assert_eq!(port_of("http://127.0.0.1/v1"), Some(80));
        assert_eq!(port_of("not a url"), None);
    }

    /// The stop command is written in the shell that will read it.
    ///
    /// Asserted per platform rather than around it: `pkill -f` reaching `cmd` is not a command
    /// that fails, it is a program `cmd` has never heard of, and a switch that cannot stop an
    /// engine is a machine that holds the memory until somebody reboots it.
    #[test]
    fn a_stop_command_speaks_the_shell_that_will_read_it() {
        let expected = if cfg!(windows) {
            "taskkill /IM ollama.exe /F"
        } else {
            "pkill -f 'ollama serve'"
        };
        assert_eq!(stop_for("ollama", "ollama serve"), expected);
    }

    /// The engine's output has to land in its log, and the log's path must never be part of a
    /// command string.
    ///
    /// This is the Windows defect written down as a test. The spawn used to build
    /// `{command} >> 'C:\...\engine.log' 2>&1 &`, and none of that means to `cmd` what it means
    /// to `sh`: a single-quoted path with a colon in it is not a redirection target at all, so
    /// the start command never ran and the log that the failure message pointed at was never
    /// created. `echo` is the one command both shells agree on.
    #[test]
    fn a_started_engine_writes_its_output_to_the_log() {
        let log = std::env::temp_dir().join(format!("flint-engine-log-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let config = Config::default();

        spawn("echo engine is starting", &config, &log).expect("the spawn itself must work");

        // The command is deliberately not waited on, so the file appears whenever the child
        // gets round to it; the deadline is what keeps that from being a race.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while std::time::Instant::now() < deadline {
            seen = std::fs::read_to_string(&log).unwrap_or_default();
            if seen.contains("engine is starting") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&log);
        assert!(
            seen.contains("engine is starting"),
            "the engine's output never reached the log: {seen:?}"
        );
    }

    /// Which provider a log belongs to, as a file name.
    #[test]
    fn a_provider_name_becomes_a_safe_file_name() {
        assert_eq!(safe_name("mlx"), "mlx");
        assert_eq!(safe_name("llama.cpp"), "llama_cpp");
        assert_eq!(safe_name("../../etc/passwd"), "______etc_passwd");
    }
}
