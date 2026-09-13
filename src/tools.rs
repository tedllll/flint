//! The tool set. Five tools, and that is the whole surface area.
//!
//! Permission model: full by default. There is exactly one switch — `readonly`
//! — exposed globally and per call, because in a rescue situation you want
//! "let it work" and "do not touch anything" and very little in between.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::Config;
use crate::context;
use crate::util;

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    async fn call(&self, args: &Value) -> Result<String>;
}

pub struct ToolBox {
    tools: Vec<Box<dyn Tool>>,
    by_name: Vec<(String, usize)>,
    /// The budget for one tool answer, after which the rest goes to a file.
    max_output: usize,
    /// Where that file goes, and how many have been written this session.
    spill_dir: PathBuf,
    spilled: std::sync::atomic::AtomicUsize,
}

impl ToolBox {
    pub fn new(config: &Config, readonly: bool, cwd: PathBuf) -> Self {
        // Skills are looked up in the same directories the prompt's catalog was built
        // from, so the tool can never offer something the catalog did not name.
        let skill_dirs = context::Workspace::discover(&cwd, &config.skill_dirs);
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(BashTool {
                config: config.clone(),
                readonly,
                cwd: cwd.clone(),
            }),
            Box::new(ReadTool { cwd: cwd.clone() }),
            Box::new(WriteTool {
                readonly,
                cwd: cwd.clone(),
            }),
            Box::new(EditTool { readonly, cwd: cwd.clone() }),
            Box::new(ListTool),
            Box::new(GlobTool { cwd: cwd.clone() }),
            Box::new(GrepTool { cwd: cwd.clone() }),
        ];
        // Offered only when there is something to load. A tool that can only ever answer
        // "no skills are configured" spends a schema on every request to say nothing.
        if !skill_dirs.skills.is_empty() {
            tools.push(Box::new(SkillTool {
                dirs: skill_dirs.skill_dirs,
            }));
        }
        let by_name = tools
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name().to_string(), i))
            .collect();
        ToolBox {
            tools,
            by_name,
            max_output: config.max_tool_output,
            spill_dir: crate::config::spill_dir().join("unattached"),
            spilled: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// File this session's spill files under a name of their own.
    pub fn with_spill_dir(mut self, dir: PathBuf) -> Self {
        self.spill_dir = dir;
        self
    }

    /// (name, description, JSON schema) for every tool, for the request body.
    pub fn specs(&self) -> Vec<(String, String, Value)> {
        self.tools
            .iter()
            .map(|t| {
                (
                    t.name().to_string(),
                    t.description().to_string(),
                    t.schema(),
                )
            })
            .collect()
    }

    pub async fn invoke(&self, name: &str, args: &Value) -> Result<String> {
        let index = self
            .by_name
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
            .ok_or_else(|| anyhow!("unknown tool '{name}'"))?;
        let output = self.tools[index].call(args).await?;
        Ok(self.cap(output))
    }

    /// Keep one tool answer inside the request budget, without pretending the rest does
    /// not exist.
    ///
    /// Cutting it off and saying "truncated" is where the information actually goes: the
    /// end of a build log is the part that says what failed. So the whole answer is
    /// written to a plain file first, and the reply keeps both ends and names that file.
    fn cap(&self, output: String) -> String {
        if output.chars().count() <= self.max_output {
            return output;
        }
        let note = match self.spill(&output) {
            Ok(path) => format!("full output: {}", path.display()),
            Err(e) => format!(
                "the whole output could not be saved in {}: {e:#}",
                self.spill_dir.display()
            ),
        };
        util::head_and_tail(&output, self.max_output, &note)
    }

    /// Write `text` to the next numbered file in this session's spill directory.
    fn spill(&self, text: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.spill_dir)
            .with_context(|| format!("cannot create {}", self.spill_dir.display()))?;
        // Numbered in the order they happened, starting at 1, so the newest file in a
        // listing is also the most recent thing that was too long to show.
        let n = self.spilled.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let path = self.spill_dir.join(format!("{n}.txt"));
        std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }

    pub fn names(&self) -> Vec<String> {
        self.by_name.iter().map(|(n, _)| n.clone()).collect()
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn resolve_path(cwd: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required string argument '{key}'"))
}

// ---------------------------------------------------------------------------
// raw command execution
//
// Shared by the `bash` tool, the REPL's `!cmd` escape and `flint exec`.
// ---------------------------------------------------------------------------

/// The `bash` tool: runs a shell command in the session working directory.
pub struct BashTool {
    config: Config,
    readonly: bool,
    cwd: PathBuf,
}

/// Resolve which shell program and arguments to actually use.
///
/// The config names one program plus the args that make it run a command
/// string. If that program is absent we fall back to another shell — and the
/// fallback gets that shell's *own* arguments, never the configured ones.
///
/// Getting this wrong is silently catastrophic: handing `cmd.exe` no `/C` makes
/// it start an interactive session, print a banner to stdout, and exit on EOF,
/// so every command appears to "succeed" while doing nothing at all. A
/// hardcoded `pwsh` is also simply absent on most Linux, macOS and plain
/// Windows machines — which is exactly where this tool matters most.
pub fn probe_shell(program: &str, args: &[String]) -> Vec<String> {
    if !program.trim().is_empty() && command_exists(program) {
        let args = if args.is_empty() {
            default_args_for(program)
        } else {
            args.to_vec()
        };
        let mut argv = vec![program.to_string()];
        argv.extend(args);
        return argv;
    }

    let candidates: &[&str] = if cfg!(windows) {
        &["cmd", "powershell", "pwsh", "bash", "sh"]
    } else {
        &["sh", "bash", "zsh", "dash"]
    };

    for candidate in candidates {
        if command_exists(candidate) {
            let mut argv = vec![candidate.to_string()];
            argv.extend(default_args_for(candidate));
            return argv;
        }
    }

    // Nothing exists. Return the platform default so the failure surfaces as a
    // real spawn error naming the missing program, rather than silently doing
    // nothing.
    let fallback = if cfg!(windows) { "cmd" } else { "sh" };
    let mut argv = vec![fallback.to_string()];
    argv.extend(default_args_for(fallback));
    argv
}

/// The arguments that make a given shell run a command string and exit.
fn default_args_for(program: &str) -> Vec<String> {
    let stem = std::path::Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| program.to_ascii_lowercase());

    if cfg!(windows) && (stem == "cmd" || stem == "cmd.exe") {
        return vec!["/C".to_string()];
    }
    if stem == "powershell" || stem == "powershell_ise" {
        return vec!["-NoProfile".to_string(), "-Command".to_string()];
    }
    if stem == "pwsh" {
        return vec!["-NoProfile".to_string(), "-Command".to_string()];
    }
    // POSIX shells: sh, bash, zsh, dash, ksh, ash, fish.
    vec!["-c".to_string()]
}

/// Is `program` runnable? Bare names are searched on `PATH`; anything with a
/// path separator is checked directly.
fn command_exists(program: &str) -> bool {
    let path = std::path::Path::new(program);
    if path.components().count() > 1 {
        return path.is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return true;
        }
        // On Windows, executables carry a PATHEXT extension.
        if cfg!(windows) {
            for ext in ["exe", "cmd", "bat", "com"] {
                if dir.join(format!("{program}.{ext}")).is_file() {
                    return true;
                }
            }
        }
        false
    })
}

/// The result of running a shell command: the text to show the user, plus the
/// real exit status.
///
/// The exit code is carried separately, and not merely embedded in `report`,
/// because callers that act as a shell (`flint exec`) must be able to exit with
/// it. A rescue tool that reports success for a failed command is worse than no
/// tool at all, and `flint exec` exists precisely to be used from scripts.
pub struct CommandOutcome {
    pub report: String,
    /// The child's exit code, or -1 when the process was killed by a signal
    /// (which has no exit code on Unix).
    pub code: i32,
}

/// What a UI implements to receive notices: anything callable with a message.
type NoticeSink = Box<dyn Fn(&str) + Send + Sync>;

/// A hook for notices that have to reach the user while a tool is still running.
///
/// A global rather than a parameter because the alternative is threading a callback
/// through every `run_command_*` signature and every call site, including the tests,
/// for the sake of one message. The notice cannot go to stderr: when the interactive
/// terminal is active, stderr lands wherever the cursor happens to be, which is inside
/// the answer strip, and it tears the layout apart.
static NOTICE: std::sync::OnceLock<NoticeSink> = std::sync::OnceLock::new();

/// Route notices to the UI. Called once, by the CLI, before the first turn.
pub fn set_notice_sink(sink: NoticeSink) {
    let _ = NOTICE.set(sink);
}

/// Where a running command's latest output line goes, when there is somewhere for it.
///
/// Separate from the notice sink on purpose. A notice is a permanent line in the
/// transcript; a download's progress is not -- one line per percent would bury the
/// conversation under its own transport. This goes to the status row instead, which
/// already exists to say what is happening right now and is repainted in place.
type ProgressSink = Box<dyn Fn(&str) + Send + Sync>;
static PROGRESS: std::sync::OnceLock<ProgressSink> = std::sync::OnceLock::new();

/// Route a running command's progress to the UI. Called once, by the CLI.
pub fn set_progress_sink(sink: ProgressSink) {
    let _ = PROGRESS.set(sink);
}

/// Report the command's most recent line of output.
///
/// Silent when nothing is listening -- a test, or a non-interactive run -- which is why
/// callers can call it unconditionally from inside the read loop.
pub fn progress(line: &str) {
    if let Some(sink) = PROGRESS.get() {
        sink(line);
    }
}

/// Report something to the user without disturbing whatever is on screen.
pub fn notice(message: &str) {
    match NOTICE.get() {
        Some(sink) => sink(message),
        // No UI has claimed the notices (a test, or an embedder): stderr is still better
        // than silence, and there is no layout to protect.
        None => eprintln!("flint: {message}"),
    }
}

impl CommandOutcome {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

/// After this long, say out loud that a command is taking a while.
///
/// Long enough that ordinary commands never trip it, short enough that a hang is
/// obvious before the reader starts wondering whether flint itself has died.
const STUCK_AFTER_SECS: u64 = 20;

/// How long a command may print nothing before it is treated as stopped.
///
/// Applies to downloads, where a total budget is the wrong instrument: a slow transfer
/// must be allowed to finish, and a dead one must not be waited on. Long enough to
/// survive a stalled chunk or a slow mirror.
const IDLE_KILL_SECS: u64 = 300;

/// Ceiling on what is kept from a command's output, per pipe.
///
/// Output is streamed as it arrives and only the report is bounded; without a cap a
/// command that prints forever would grow the string until the process ran out of memory,
/// because nothing else limits what a child may write.
const MAX_CAPTURE: usize = 4 * 1024 * 1024;

/// How often to report progress for a download that emits none of its own.
///
/// Frequent enough to be reassuring, rare enough that the status row is readable and the
/// loop is not spinning.
const PROGRESS_REPORT_SECS: u64 = 10;

/// Budget for a download, which is bounded by idleness rather than by this.
const DOWNLOAD_BASH_TIMEOUT: u64 = 2 * 3600;

/// Default budget for an ordinary command.
const DEFAULT_BASH_TIMEOUT: u64 = 120;

/// Budget for a command that installs, builds or downloads, which is slow by nature.
const LONG_BASH_TIMEOUT: u64 = 900;

/// Whether a command is the kind that legitimately takes minutes.
///
/// Deliberately generous and deliberately dumb: a false positive costs a longer wait
/// before the kill, while a false negative kills a package install halfway through and
/// leaves the toolchain in a worse state than before.
fn looks_slow(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "install", "update", "upgrade", "upgrade", "build", "fetch", "clone", "download",
        "cargo", "npm", "pnpm", "yarn", "pip", "winget", "choco", "scoop", "apt", "dnf",
        "yum", "pacman", "brew", "rustup", "docker", "msbuild", "gradle", "mvn", "make",
        "cmake", "dist-upgrade", "system-upgrade",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// Whether a command is fetching something over the network.
///
/// Detection is deliberately literal and conservative, because the cost of a wrong
/// answer is asymmetric. Treating an ordinary command as a download only means it gets a
/// longer leash and its output is echoed to the status row -- harmless. Treating a
/// download as ordinary kills it at two minutes and loses the transfer, which is the
/// complaint this exists to answer.
///
/// Only the *fetching forms* count, not the tool names: `git status` and `pip list` are
/// not downloads, while `git clone` and `pip install` are, and a bare `npm` or `cargo`
/// says nothing either way. Flagging a whole tool would make `cargo build` -- which
/// compiles locally and can take ten minutes for other reasons -- look like a transfer.
///
/// The model can say so outright with the `download` argument when this misses, which is
/// the escape hatch for a vendored script or a tool nobody has heard of.
fn looks_like_download(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let has = |w: &str| words.contains(&w);
    let any = |list: &[&str]| words.iter().any(|x| list.contains(x));

    // Fetching tools, in their fetching forms.
    if has("curl") {
        // A plain `curl URL` prints to stdout and is a fetch too; the flags that make it
        // a plain read (`-I`, `-sS -o /dev/null`) are still network calls, so anything
        // with curl counts.
        return true;
    }
    if has("wget") || has("aria2c") || has("axel") || has("rsync") || has("scp") || has("sftp") {
        return true;
    }
    // Package managers: the verbs that fetch, not the ones that list.
    if any(&["install", "add", "upgrade", "update", "fetch", "download", "pull", "sync"])
        && any(&["npm", "pnpm", "yarn", "pip", "pip3", "poetry", "uv", "gem", "go", "cargo",
                "brew", "apt", "apt-get", "dnf", "yum", "pacman", "apk", "choco", "winget",
                "scoop", "nix", "conda", "mamba", "docker", "helm", "rustup"])
    {
        return true;
    }
    // `git` fetches only for these verbs.
    if has("git") && any(&["clone", "fetch", "pull", "submodule"]) {
        return true;
    }
    // `pip download`, `npm pack`, and friends are covered above by the verb list.
    false
}

/// `12s`, `1m 05s`, `1h 02m` -- short enough for a one-line notice.
fn elapsed_label(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Run a shell command and return its combined output, including a trailing
/// `[exit code: N]` marker when it failed.
///
/// This is the agent-facing wrapper: the model only needs the text, so the exit
/// code is dropped here. Callers that must propagate the status should use
/// [`run_command_detailed`] instead.
pub async fn run_command_raw(
    config: &Config,
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> Result<String> {
    Ok(run_command_detailed(config, command, cwd, timeout_secs)
        .await?
        .report)
}

/// Run a shell command, returning both the combined output and the exit code.
pub async fn run_command_detailed(
    config: &Config,
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> Result<CommandOutcome> {
    run_command_streaming(config, command, cwd, timeout_secs, false).await
}

/// Run a command, reporting its output as it arrives.
///
/// `idle_kill` changes what the clock means. An ordinary command is bounded by its total
/// timeout, because nothing about it is expected to take long. A download is not: it may
/// legitimately take an hour at a slow link, and the interesting question is not how long
/// it has run but whether it is still moving. With `idle_kill` set, output resets the
/// clock and a transfer that has gone quiet is killed -- which a total budget cannot
/// express, since it kills the slow and tolerates the dead.
pub async fn run_command_streaming(
    config: &Config,
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
    idle_kill: bool,
) -> Result<CommandOutcome> {
    let shell = probe_shell(&config.shell, &config.shell_args);
    let mut cmd = tokio::process::Command::new(&shell[0]);
    // Arguments are passed as argv, never concatenated into one string: that
    // avoids a second round of quoting rules on Windows.
    for arg in &shell[1..] {
        cmd.arg(arg);
    }
    cmd.arg(command);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Let a configured proxy reach commands flint runs for us. Without this the
    // tool cannot repair anything that needs to download, on a network where
    // direct access is blocked but a local proxy works.
    if let Some(proxy) = config.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        cmd.env("HTTP_PROXY", proxy);
        cmd.env("HTTPS_PROXY", proxy);
        cmd.env("http_proxy", proxy);
        cmd.env("https_proxy", proxy);
        cmd.env("ALL_PROXY", proxy);
        cmd.env("all_proxy", proxy);
    }

    // `kill_on_drop` as well as the explicit kill below. The explicit one is what
    // actually reaps the process; this is the backstop for every *other* way the future
    // can end -- an interrupt drops the turn, and a dropped turn must not leave a build
    // running behind it.
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot spawn shell '{}'", shell[0]))?;

    // Both pipes are read while the command runs, and the lines are funnelled through one
    // channel. `wait_with_output` cannot do this: it returns only when the child has
    // finished, which is precisely the moment progress is no longer interesting.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Line>();
    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        readers.push(tokio::spawn(pump_lines(pipe, tx.clone(), true)));
    }
    if let Some(pipe) = child.stderr.take() {
        readers.push(tokio::spawn(pump_lines(pipe, tx.clone(), false)));
    }
    drop(tx);

    let total = Duration::from_secs(timeout_secs);
    let mut stuck_at_override: Option<Duration> = None;
    let started = std::time::Instant::now();
    let mut warned = false;
    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    let mut last_output = std::time::Instant::now();
    // Reported progress of our own, for a download that emits none; kept apart from
    // `last_output` because a report must not be mistaken for the transfer moving.
    let mut last_report: Option<std::time::Instant> = None;

    // Each pass either yields a line, or ends the loop with the child's status.
    let status = loop {
        let stuck_at = stuck_at_override.unwrap_or_else(|| Duration::from_secs(STUCK_AFTER_SECS));
        // Measured from the last report when one has been made, so re-arming waits a full
        // interval instead of firing again on the next pass.
        let until_stuck = match last_report {
            Some(at) => stuck_at.saturating_sub(at.elapsed()),
            None => stuck_at.saturating_sub(started.elapsed()),
        };
        let until_kill = total.saturating_sub(started.elapsed());
        let idle_deadline = Duration::from_secs(IDLE_KILL_SECS);
        let until_idle = idle_deadline.saturating_sub(last_output.elapsed());

        enum Wake {
            Line(Option<Line>),
            Exited(std::io::Result<std::process::ExitStatus>),
            TimedOut,
            WentQuiet,
            Stuck,
        }

        let wake = tokio::select! {
            line = rx.recv() => Wake::Line(line),
            status = child.wait() => Wake::Exited(status),
            _ = tokio::time::sleep(until_kill) => Wake::TimedOut,
            // Only a download gets the idleness rule; see the doc comment.
            _ = tokio::time::sleep(until_idle), if idle_kill => Wake::WentQuiet,
            _ = tokio::time::sleep(until_stuck), if !warned => Wake::Stuck,
        };

        match wake {
            Wake::Line(Some(Line { text, stdout })) => {
                last_output = std::time::Instant::now();
                progress(&text);
                if stdout {
                    if stdout_text.len() < MAX_CAPTURE {
                        stdout_text.push_str(&text);
                        stdout_text.push('\n');
                    }
                } else if stderr_text.len() < MAX_CAPTURE {
                    stderr_text.push_str(&text);
                    stderr_text.push('\n');
                }
            }
            // Every sender is gone, so both readers have finished -- but the child may
            // have exited without either pipe reaching EOF first, so ask for its status
            // rather than assuming.
            Wake::Line(None) => {
                break child.wait().await.context("cannot reap the command")?;
            }
            Wake::Exited(status) => {
                break status.context("failed to collect command output")?;
            }
            Wake::TimedOut => {
                return Err(anyhow!(
                    "killed after {timeout_secs}s with no result. If it is genuinely slow, pass a \
                     larger timeout_secs or write [timeout:N] before the command; if it is waiting \
                     for input, it never will -- stdin is closed."
                ));
            }
            Wake::WentQuiet => {
                return Err(anyhow!(
                    "no output for {IDLE_KILL_SECS}s, so it was killed. A command that keeps \
                     printing is left alone however long it takes; one that has gone quiet has \
                     usually stopped for good."
                ));
            }
            Wake::Stuck => {
                warned = true;
                if idle_kill {
                    // A download that says nothing is not necessarily stuck: `curl` and
                    // `wget` suppress their own progress bars when stderr is not a
                    // terminal, which it never is here, so a perfectly healthy transfer is
                    // silent for its whole duration. Saying so -- and how big the output
                    // has got -- is the only progress available in that case, and it beats
                    // a status line that just counts seconds.
                    progress(&format!(
                        "downloading, {} so far{}",
                        elapsed_label(started.elapsed()),
                        output_size_note(command)
                    ));
                    // Re-arm, but only after a decent interval, and *not* by resetting the
                    // idleness baseline: that is what decides whether the transfer is
                    // dead, and a report of our own is not evidence of life.
                    warned = false;
                    stuck_at_override = Some(std::time::Duration::from_secs(PROGRESS_REPORT_SECS));
                    last_report = Some(std::time::Instant::now());
                } else {
                    notice(&format!(
                        "this command has been running for {} and may be stuck. It is killed at \
                         {timeout_secs}s; pass a larger `timeout_secs`, or write [timeout:N] before \
                         the command, if it is genuinely slow.",
                        elapsed_label(started.elapsed())
                    ));
                }
            }
        }
    };

    // The readers may still be delivering the tail of the output after the child exits.
    // A short grace period collects it, so a command's last lines are not lost at the
    // finish line -- which is exactly where the interesting error message usually is.
    for reader in readers {
        let _ = reader.await;
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(20), rx.recv()).await {
            Ok(Some(Line { text, stdout })) => {
                if stdout {
                    stdout_text.push_str(&text);
                    stdout_text.push('\n');
                } else {
                    stderr_text.push_str(&text);
                    stderr_text.push('\n');
                }
            }
            _ => break,
        }
    }

    let stdout = util::sanitize_output(stdout_text.trim_end());
    let stderr = util::sanitize_output(stderr_text.trim_end());
    let code = status.code().unwrap_or(-1);

    let mut report = String::new();
    if !stdout.trim().is_empty() {
        report.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        if !report.is_empty() {
            report.push_str("\n--- stderr ---\n");
        }
        report.push_str(stderr.trim_end());
    }
    if report.trim().is_empty() {
        report.push_str("(no output)");
    }
    if code != 0 {
        report.push_str(&format!("\n[exit code: {code}]"));
    }
    Ok(CommandOutcome { report, code })
}

/// How large the file a download is writing to has become, if it can be told.
///
/// Best effort by design. Finding the destination means reading the command line, and a
/// command line can put it anywhere -- `-o`, `-O`, a redirect, a tool that names its own
/// temporary file. When it cannot be found the total is simply absent, which is better
/// than guessing and reporting the size of something unrelated.
fn output_size_note(command: &str) -> String {
    for token in ["-o ", "--output ", "-O "] {
        if let Some(rest) = command.split(token).nth(1) {
            let path = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(['\'', '"']);
            if path.is_empty() {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(path) {
                return format!(", {} written to {}", human_bytes(meta.len()), path);
            }
        }
    }
    String::new()
}

/// Bytes as something a person reads at a glance.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// One line of a running command's output, and which pipe it came from.
struct Line {
    text: String,
    stdout: bool,
}

/// Read a pipe line by line, forwarding each complete line. Never returns an error: a
/// pipe that fails to read is the command's ending, not the read's problem.
async fn pump_lines<R>(pipe: R, tx: tokio::sync::mpsc::UnboundedSender<Line>, stdout: bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    // Read bytes, not lines, and cut them here.
    //
    // `read_until(b'\n')` reads until it *sees* a newline, so a progress bar -- which
    // redraws one line with a carriage return and emits no newline until it is finished --
    // produces nothing at all for the whole transfer. The download stalled rather than
    // merely going unreported, because the reader was waiting and the pipe filled.
    let mut reader = pipe;
    let mut buf = [0u8; 8192];
    // A read ends wherever the pipe ends, not where a line does: a chunk boundary can
    // fall in the middle of a progress redraw, and emitting the halves separately would
    // put half a percentage on screen twice. The tail is held until its terminator
    // arrives, or until the pipe closes and it is all there is.
    let mut carry: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                carry.extend_from_slice(&buf[..n]);
                let mut start = 0usize;
                for i in 0..carry.len() {
                    if carry[i] == b'\n' || carry[i] == b'\r' {
                        if let Some(text) = clean_piece(&carry[start..i]) {
                            if tx.send(Line { text, stdout }).is_err() {
                                return;
                            }
                        }
                        start = i + 1;
                    }
                }
                carry.drain(..start);
            }
            Err(_) => break,
        }
    }
    if let Some(text) = clean_piece(&carry) {
        let _ = tx.send(Line { text, stdout });
    }
}

/// One piece of a command's output, or `None` when it says nothing.
///
/// A redraw is frequently a row of spaces followed by a carriage return. Reporting that
/// as the command's current activity would replace a useful line with nothing.
fn clean_piece(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Split a chunk on both terminators, dropping the pieces that say nothing.
///
/// Kept beside `clean_piece` so the rule is stated once and can be tested without a
/// pipe: `\r` ends a line exactly as `\n` does, which is what makes a progress bar
/// visible at all.
#[cfg(test)]
fn split_progress_lines(buf: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(buf)
        .split(['\n', '\r'])
        .filter_map(|s| {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        })
        .collect()
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command on the local machine and return its combined output. \
         This is the primary way to inspect and repair the system (package \
         managers, git, cargo, npm, service status). Working directory is \
         preserved across calls within a session."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command line to execute." },
                "download": {
                    "type": "boolean",
                    "description": "Set when the command fetches something over the network and \\
                         the command line does not make that obvious. A download is not killed on \\
                         a total time budget; it is killed only if it stops producing output."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": format!(
                        "Kill the command after this many seconds (default {DEFAULT_BASH_TIMEOUT} \
                         for ordinary commands, {LONG_BASH_TIMEOUT} when the command installs, \
                         builds or downloads). Raise it for anything known to be slow."
                    )
                }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let command = require_str(args, "command")?;
        // The model may say a command is a download when the literal check cannot tell:
        // a vendored script, a wrapper, a tool nobody has heard of. It is an *addition*
        // to the heuristic, not a replacement for it -- relying on the model to remember
        // a flag would make the behaviour depend on the model's diligence, and a missed
        // flag would silently restore the two-minute kill.
        let declared = args
            .get("download")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let download = declared || looks_like_download(command);

        let timeout = match args.get("timeout_secs").and_then(Value::as_u64) {
            Some(explicit) => explicit,
            // A download is bounded by idleness rather than by a total budget: it may
            // legitimately take an hour, and what matters is whether it is still moving.
            None if download => DOWNLOAD_BASH_TIMEOUT,
            // A command that installs or builds is expected to be slow, and killing
            // `cargo install` at two minutes is not a safety feature -- it is a false
            // alarm that teaches the model to work around the tool.
            None if looks_slow(command) => LONG_BASH_TIMEOUT,
            None => DEFAULT_BASH_TIMEOUT,
        }
        .clamp(1, 24 * 3600);

        if self.readonly && !is_readonly_command(command) {
            return Err(anyhow!(
                "readonly mode is ON: refusing to run `{}`. \
                 Turn it off with /readonly, or use a read-only command.",
                util::preview(command, 120)
            ));
        }

        let outcome =
            run_command_streaming(&self.config, command, &self.cwd, timeout, download).await?;
        Ok(util::truncate(&outcome.report, self.config.max_tool_output))
    }
}

/// Conservative read-only classifier.
///
/// Two rules keep this honest:
///   * shell metacharacters (`>`, `&&`, `;`, `sudo`) always mean "not safe",
///     because they compose arbitrary behaviour out of innocent-looking parts;
///   * subcommand-aware tools (`git`, `npm`, `systemctl`, `docker`) are judged
///     on their *first two* words, so `git status` is inspection while
///     `git commit` is mutation.
///
/// Anything unrecognised is treated as NOT read-only. A false "safe" is far
/// more expensive than a false "unsafe", and the user can always drop
/// `/readonly`.
pub fn is_readonly_command(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let stripped = command.trim().to_string();
    let lower = stripped.to_ascii_lowercase();

    // Composition and privilege escalation always disqualify a command.
    for bad in [">", ">>", "&&", "||", ";", "|", "`", "$(", "sudo", "doas"] {
        if lower.contains(bad) {
            return false;
        }
    }
    // `sed -i` rewrites files in place.
    if lower.contains("sed -i") {
        return false;
    }

    let words: Vec<&str> = lower.split_whitespace().collect();
    let Some(first) = words.first().copied() else {
        return false;
    };

    const READ_ONLY: [&str; 31] = [
        "ls", "cat", "head", "tail", "less", "more", "pwd", "echo", "whoami", "id", "date", "env",
        "printenv", "which", "whereis", "type", "file", "stat", "wc", "grep", "rg", "find", "fd",
        "tree", "ps", "top", "df", "du", "uname", "hostname", "git",
    ];
    if READ_ONLY.contains(&first) {
        // `git` is the only entry here that can also mutate, so check the verb.
        if first == "git" {
            return git_is_read_only(words.get(1).copied());
        }
        return true;
    }

    let second = words.get(1).copied().unwrap_or("");

    // Service and container inspection.
    if first == "systemctl" {
        return matches!(
            second,
            "status" | "show" | "list-units" | "list-unit-files" | "cat"
        );
    }
    if first == "docker" {
        return matches!(
            second,
            "ps" | "images" | "logs" | "inspect" | "version" | "info"
        );
    }

    // Package managers: listing installed state is inspection, everything else
    // (install, update, remove) is a system change.
    if matches!(first, "npm" | "pnpm" | "yarn") {
        return matches!(second, "ls" | "list" | "view" | "why" | "outdated");
    }
    if first == "cargo" {
        return matches!(second, "tree" | "metadata" | "search");
    }
    if matches!(first, "pip" | "pip3") {
        return matches!(second, "list" | "show" | "freeze");
    }

    false
}

/// Read-only git verbs. Anything not listed (commit, push, checkout, reset,
/// clean, ...) is treated as mutation.
fn git_is_read_only(subcommand: Option<&str>) -> bool {
    match subcommand {
        Some(cmd) => matches!(
            cmd,
            "status"
                | "log"
                | "diff"
                | "show"
                | "branch"
                | "remote"
                | "describe"
                | "rev-parse"
                | "ls-files"
                | "blame"
                | "tag"
                | "config"
                | "shortlog"
                | "whatchanged"
        ),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

pub struct ReadTool {
    cwd: PathBuf,
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a text file and return it with line numbers prefixed. \
         Use `offset` and `limit` to page through long files."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path (absolute, or relative to the working directory)." },
                "offset": { "type": "integer", "description": "1-based first line to return." },
                "limit": { "type": "integer", "description": "Maximum number of lines (default 2000)." }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let path = resolve_path(&self.cwd, require_str(args, "path")?);
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(2000) as usize;

        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("cannot read {}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);

        let total = text.lines().count();
        let body: Vec<String> = text
            .lines()
            .skip(offset - 1)
            .take(limit)
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", offset + i, line))
            .collect();

        if body.is_empty() {
            return Ok(format!(
                "(no content: file has {total} lines, requested offset {offset})"
            ));
        }

        let mut out = body.join("\n");
        if offset - 1 + limit < total {
            out.push_str(&format!(
                "\n\n... [{} more lines; continue with offset={}]",
                total - (offset - 1 + limit),
                offset + limit
            ));
        }
        Ok(util::truncate(&out, 60_000))
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

pub struct WriteTool {
    readonly: bool,
    cwd: PathBuf,
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create or completely overwrite a text file. Parent directories are \
         created automatically. For small changes prefer `edit`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to write." },
                "content": { "type": "string", "description": "Full file content." }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        if self.readonly {
            return Err(anyhow!(
                "readonly mode is ON: refusing to write files. Turn it off with /readonly."
            ));
        }
        let path = resolve_path(&self.cwd, require_str(args, "path")?);
        let content = require_str(args, "content")?;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
        }
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(format!(
            "wrote {} ({} bytes)",
            path.display(),
            content.len()
        ))
    }
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

pub struct EditTool {
    readonly: bool,
    cwd: PathBuf,
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. `old_string` must match byte for byte \
         and, unless `replace_all` is true, must appear exactly once."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to edit." },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        if self.readonly {
            return Err(anyhow!(
                "readonly mode is ON: refusing to edit files. Turn it off with /readonly."
            ));
        }
        let path = resolve_path(&self.cwd, require_str(args, "path")?);
        let old = require_str(args, "old_string")?;
        let new = require_str(args, "new_string")?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if old.is_empty() {
            return Err(anyhow!("old_string must not be empty"));
        }

        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("cannot read {}", path.display()))?;

        let count = text.matches(old).count();
        if count == 0 {
            return Err(anyhow!(
                "old_string not found in {}. Read the file first to get the exact text.",
                path.display()
            ));
        }
        if count > 1 && !replace_all {
            return Err(anyhow!(
                "old_string appears {count} times in {}. Add more context to make it \
                 unique, or set replace_all=true.",
                path.display()
            ));
        }

        let updated = if replace_all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };

        tokio::fs::write(&path, updated)
            .await
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(format!(
            "edited {} ({count} replacement{})",
            path.display(),
            if count == 1 { "" } else { "s" }
        ))
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

pub struct ListTool;

#[async_trait::async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn description(&self) -> &str {
        "List directory entries (files and subdirectories) to orient yourself in \
         the filesystem."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path (default '.')." }
            }
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        // The list tool is always allowed, even in readonly mode.
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let path = PathBuf::from(raw);
        let mut entries = tokio::fs::read_dir(&path)
            .await
            .with_context(|| format!("cannot list {}", path.display()))?;

        let mut lines = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await.ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let name = entry.file_name().to_string_lossy().to_string();
            if is_dir {
                lines.push(format!("{name}/"));
            } else {
                lines.push(format!("{name}  ({size} bytes)"));
            }
        }
        lines.sort();
        if lines.is_empty() {
            return Ok(format!("{} is empty", path.display()));
        }
        Ok(util::truncate(&lines.join("\n"), 20_000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_classifier_allows_inspection() {
        for cmd in [
            "ls -la",
            "cat /etc/hosts",
            "ps aux",
            "df -h",
            "find . -name '*.rs'",
            "grep -rn foo .",
            "systemctl status nginx",
            "docker ps",
            "npm ls -g",
            "cargo tree",
            // Inspection subcommands of a tool that can also mutate.
            "git status",
            "git log --oneline -20",
            "git diff HEAD",
            "git remote -v",
            "git rev-parse --show-toplevel",
            "git branch -a",
        ] {
            assert!(is_readonly_command(cmd), "{cmd:?} should be read-only");
        }
    }

    #[test]
    fn readonly_classifier_blocks_mutation() {
        for cmd in [
            "rm -rf /tmp/x",
            "mv a b",
            "cp a b",
            "echo hi > file",
            "cat a >> b",
            "sudo apt install curl",
            "npm i -g foo",
            "npm install",
            "git commit -m x",
            "git push",
            "git checkout main",
            "git clean -fd",
            "git reset --hard",
            "git", // no subcommand: cannot prove it is safe
            "cargo install flint",
            "cargo build --release",
            "pip install requests",
            "systemctl restart nginx",
            "docker rm container",
            "sed -i 's/a/b/' file",
            "ls && rm x",
            "ls | tee f",
            "ls; rm x",
            "cat $(find . -name secret)",
            "ls `whoami`",
            "apt install vim",
            "chmod 777 /tmp/x",
            "kill -9 123",
            "make install",
        ] {
            assert!(!is_readonly_command(cmd), "{cmd:?} must NOT be read-only");
        }
    }

    #[test]
    fn readonly_classifier_rejects_empty() {
        assert!(!is_readonly_command(""));
        assert!(!is_readonly_command("   "));
    }

    #[test]
    fn shell_probe_finds_something_usable() {
        // Whatever platform this runs on, a shell must be found: a rescue tool
        // that cannot start a shell is useless. A bogus program must still fall
        // through to a working default.
        for (program, args) in [
            ("definitely-not-a-real-shell-xyz", vec![]),
            ("pwsh", vec![]),
            ("", vec![]),
        ] {
            let shell = probe_shell(program, &args);
            assert!(!shell.is_empty(), "probe must return argv");
            assert!(
                command_exists(&shell[0]),
                "probe returned a shell that does not exist: {shell:?}"
            );
        }
    }

    /// Regression test for a bug that made every command silently do nothing.
    ///
    /// `cmd.exe` without `/C` starts an interactive session: it prints a banner
    /// to stdout and exits on EOF. The command is never run, and because the
    /// exit status is 0 it looks like success.
    #[test]
    fn a_resolved_shell_always_gets_its_execute_flag() {
        // The configured program must keep its configured args.
        if cfg!(windows) {
            let shell = probe_shell("cmd", &["/C".to_string()]);
            assert_eq!(shell, vec!["cmd", "/C"]);
        } else {
            let shell = probe_shell("sh", &["-c".to_string()]);
            assert_eq!(shell, vec!["sh", "-c"]);
        }

        // A fallback shell must get ITS OWN flag, never the configured one and
        // never none at all.
        let fallback = probe_shell("definitely-not-a-real-shell-xyz", &[]);
        let program = fallback[0].to_ascii_lowercase();
        let stem = std::path::Path::new(&program)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(program.clone());
        let expected = if stem.contains("cmd") {
            "/C"
        } else if stem.contains("powershell") || stem.contains("pwsh") {
            "-Command"
        } else {
            "-c"
        };
        assert!(
            fallback.iter().any(|a| a == expected),
            "shell {fallback:?} is missing its execute flag {expected:?}; \
             without it the shell goes interactive and runs nothing"
        );
    }

    #[test]
    fn shell_probe_uses_the_configured_program_when_present() {
        let (program, args, flag) = if cfg!(windows) {
            ("cmd", "/C", "/C")
        } else {
            ("sh", "-c", "-c")
        };
        let shell = probe_shell(program, &[args.to_string()]);
        assert_eq!(shell.first().map(String::as_str), Some(program));
        assert_eq!(shell.get(1).map(String::as_str), Some(flag));
    }

    #[test]
    fn empty_shell_args_are_filled_in_per_program() {
        // A config that names only the program must still work.
        if cfg!(windows) {
            assert_eq!(probe_shell("cmd", &[]), vec!["cmd", "/C"]);
        } else {
            assert_eq!(probe_shell("sh", &[]), vec!["sh", "-c"]);
        }
    }

    #[test]
    fn resolve_path_joins_relative_and_keeps_absolute() {
        let cwd = Path::new("/work");
        assert_eq!(resolve_path(cwd, "a/b"), PathBuf::from("/work/a/b"));
        assert_eq!(resolve_path(cwd, "/etc/hosts"), PathBuf::from("/etc/hosts"));
    }

    #[tokio::test]
    async fn bash_tool_refuses_mutation_in_readonly() {
        // The guard must live inside the tool, not only in the UI.
        let tool = BashTool {
            config: Config::default(),
            readonly: true,
            cwd: PathBuf::from("."),
        };
        let args = json!({ "command": "rm -rf /tmp/definitely-not-real" });
        let result = tool.call(&args).await;
        assert!(
            result.is_err(),
            "readonly bash must refuse a mutating command"
        );
        assert!(
            format!("{:#}", result.unwrap_err()).contains("readonly"),
            "the refusal must explain itself"
        );
    }
}

// ---------------------------------------------------------------------------
// recursive traversal, shared by `glob` and `grep`
// ---------------------------------------------------------------------------

/// Directories never worth walking into.
///
/// `.git` and `node_modules` are the difference between a search that answers in
/// milliseconds and one that never finishes -- and neither is what the user meant.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "__pycache__"];

/// Cap on entries returned, so one search cannot flood the model's context.
const WALK_LIMIT: usize = 200;

/// Every file under `root`, as paths relative to it, depth-first and sorted.
///
/// Written out rather than pulled in: walking a directory tree is twenty lines, and this
/// project's whole reason for existing is that it must run where package managers and
/// toolchains do not. A `glob`/`walkdir` dependency would be the one thing standing
/// between a broken machine and a working search.
fn walk_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // An unreadable directory is normal (permissions, races) and must not abort
            // the whole search: partial results beat no results when repairing a system.
            continue;
        };
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                subdirs.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= limit {
                    return out;
                }
            }
        }
        // Reverse so the depth-first order is alphabetical rather than stack order.
        subdirs.sort_by(|a, b| b.cmp(a));
        stack.extend(subdirs);
    }
    out.sort();
    out
}

/// Whether `path` matches a glob pattern of the `**`, `*` and `?` kind.
///
/// Supports the three wildcards that matter for finding files and nothing else. `*` and
/// `?` stop at a `/`; `**` crosses them. Matching is against the `/`-separated relative
/// path, so it behaves the same on Windows as on Unix -- which is the entire point, since
/// the shell on one of those platforms has no `find` to fall back on.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = path.chars().collect();
    glob_here(&pat, &txt)
}

fn glob_here(pat: &[char], txt: &[char]) -> bool {
    // Walk the pattern, consuming text. `**` is the only case that needs branching, and
    // it is handled by trying every split point.
    let mut pi = 0usize;
    let mut ti = 0usize;
    while pi < pat.len() {
        match pat[pi] {
            '*' => {
                let double = pat.get(pi + 1) == Some(&'*');
                let mut next = pi + if double { 2 } else { 1 };
                // `**/` may also match nothing, so `**/foo` matches a bare `foo`.
                let allow_empty_dir = double && pat.get(next) == Some(&'/');
                if allow_empty_dir {
                    next += 1;
                }
                if next >= pat.len() {
                    // Trailing `*` matches the rest of this segment; trailing `**` matches
                    // everything.
                    return double || !txt[ti..].contains(&'/');
                }
                // Try the shortest match first, so `*` stays inside one path segment
                // unless the pattern's remainder forces it to extend.
                let mut k = ti;
                loop {
                    if glob_here(&pat[next..], &txt[k..]) {
                        return true;
                    }
                    if k >= txt.len() {
                        break;
                    }
                    if !double && txt[k] == '/' {
                        break;
                    }
                    k += 1;
                }
                if allow_empty_dir && glob_here(&pat[next..], &txt[ti..]) {
                    return true;
                }
                return false;
            }
            '?' => {
                if ti >= txt.len() || txt[ti] == '/' {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= txt.len() || txt[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == txt.len()
}

/// Does the pattern look like it is meant to match anywhere in the tree?
///
/// `*.rs` clearly means "all Rust files" rather than "a file named `*.rs` in the cwd",
/// so a pattern with no separator is also tried prefixed with `**/`. Guessing otherwise
/// makes the common case return nothing, which reads as "no such file" and sends the
/// model down a wrong path.
fn pattern_needs_prefix(pattern: &str) -> bool {
    !pattern.contains('/') && (pattern.contains('*') || pattern.contains('?'))
}

// ---------------------------------------------------------------------------
// skill
// ---------------------------------------------------------------------------

/// The `skill` tool: load one skill's full instructions.
///
/// The prompt carries summaries only, and this is how the body arrives. Nothing is
/// cached, so the directory is read on every call: a skill written or edited while flint
/// is running is available on the next call, which is the same promise the rest of the
/// tool set makes about the filesystem.
pub struct SkillTool {
    dirs: Vec<PathBuf>,
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions of a skill named in the system prompt's catalog. \
         Call this before following a skill: the catalog carries only its summary. The \
         body is re-read from disk on every call."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name, exactly as the catalog spells it."
                }
            },
            "required": ["name"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let name = require_str(args, "name")?;
        context::load_skill(&self.dirs, name.trim())
    }
}

// ---------------------------------------------------------------------------
// glob
// ---------------------------------------------------------------------------

pub struct GlobTool {
    cwd: PathBuf,
}

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by name pattern, searching recursively. Use this instead of \
         shelling out: `*.rs` finds every Rust file below the path, and `**/test_*.py` \
         matches at any depth. This is the tool for \"where is that file\"."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern. `*` and `?` stop at a path separator, `**` crosses them. A pattern with no `/` is matched at every depth."
                },
                "path": { "type": "string", "description": "Directory to search (default '.')." }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let pattern = require_str(args, "pattern")?;
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = resolve_path(&self.cwd, raw);

        let files = walk_files(&root, WALK_LIMIT * 10);
        let mut hits: Vec<String> = files
            .into_iter()
            .filter(|p| {
                glob_match(pattern, p)
                    || (pattern_needs_prefix(pattern) && glob_match(&format!("**/{pattern}"), p))
            })
            .collect();
        hits.truncate(WALK_LIMIT);

        if hits.is_empty() {
            return Ok(format!("no files matching '{pattern}' under {}", root.display()));
        }
        let mut out = hits.join("\n");
        out.push_str(&format!("\n({} file(s))", hits.len()));
        Ok(util::truncate(&out, 20_000))
    }
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

pub struct GrepTool {
    cwd: PathBuf,
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file *contents* for a literal string, recursively, and return matches \
         with file names and line numbers. This is the tool for \"where is this used\". \
         Narrow the search with `path` and `glob` when the tree is large."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Literal text to find (not a regular expression)." },
                "path": { "type": "string", "description": "Directory or file to search (default '.')." },
                "glob": { "type": "string", "description": "Only search files matching this pattern, e.g. `*.rs`." },
                "ignore_case": { "type": "boolean", "description": "Case-insensitive search." }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let needle = require_str(args, "pattern")?;
        if needle.is_empty() {
            return Err(anyhow!("grep pattern must not be empty"));
        }
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = resolve_path(&self.cwd, raw);
        let file_glob = args.get("glob").and_then(Value::as_str);
        let ignore_case = args
            .get("ignore_case")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // A single file is the common case when following up on a `glob` result.
        let candidates: Vec<String> = if root.is_file() {
            vec![String::new()]
        } else {
            walk_files(&root, WALK_LIMIT * 20)
        };

        let hay = if ignore_case { needle.to_lowercase() } else { needle.to_string() };
        let mut out: Vec<String> = Vec::new();
        let mut matched_files = 0usize;
        let mut truncated = false;

        for rel in candidates {
            let path = if rel.is_empty() { root.clone() } else { root.join(&rel) };
            if let Some(g) = file_glob {
                let target = if rel.is_empty() {
                    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                } else {
                    rel.clone()
                };
                if !glob_match(g, &target)
                    && !(pattern_needs_prefix(g) && glob_match(&format!("**/{g}"), &target))
                {
                    continue;
                }
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                // Binary or unreadable: skip rather than fail. A grep across a project
                // hits both constantly, and neither is an error worth reporting.
                continue;
            };
            let mut file_hits = 0usize;
            for (n, line) in text.lines().enumerate() {
                let hit = if ignore_case {
                    line.to_lowercase().contains(&hay)
                } else {
                    line.contains(&hay)
                };
                if hit {
                    file_hits += 1;
                    if out.len() < 300 {
                        out.push(format!(
                            "{}:{}: {}",
                            if rel.is_empty() { path.display().to_string() } else { rel.clone() },
                            n + 1,
                            util::truncate(line.trim(), 200)
                        ));
                    } else {
                        truncated = true;
                    }
                }
            }
            if file_hits > 0 {
                matched_files += 1;
            }
        }

        if out.is_empty() {
            return Ok(format!("no matches for '{needle}' under {}", root.display()));
        }
        let mut text = out.join("\n");
        if truncated {
            text.push_str("\n... more matches not shown");
        }
        text.push_str(&format!("\n({} match(es) in {} file(s))", out.len(), matched_files));
        Ok(util::truncate(&text, 20_000))
    }
}

#[cfg(test)]
mod glob_tests {
    use super::*;

    #[test]
    fn a_star_stays_inside_one_path_segment() {
        assert!(glob_match("*.rs", "main.rs"));
        // The classic bug: `*` leaking across separators, so a root-level pattern
        // matches files in subdirectories and the model gets paths it did not ask for.
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/deep/main.rs"));
    }

    #[test]
    fn a_double_star_crosses_separators() {
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "a/b/c/main.rs"));
        assert!(glob_match("**/*.rs", "main.rs"), "`**/` must also match zero directories");
        assert!(!glob_match("**/*.rs", "src/main.py"));
    }

    #[test]
    fn a_double_star_in_the_middle_matches_any_depth() {
        assert!(glob_match("src/**/test.rs", "src/test.rs"));
        assert!(glob_match("src/**/test.rs", "src/a/test.rs"));
        assert!(glob_match("src/**/test.rs", "src/a/b/test.rs"));
        assert!(!glob_match("src/**/test.rs", "other/a/test.rs"));
    }

    #[test]
    fn a_bare_pattern_is_tried_at_every_depth() {
        // What the model means by `*.rs` is almost never "a Rust file sitting in the
        // current directory" -- and returning nothing for the obvious request reads as
        // "there is no such file", which sends it down a wrong path.
        assert!(pattern_needs_prefix("*.rs"));
        assert!(pattern_needs_prefix("test_?.py"));
        // An explicit path is a deliberate narrowing and must be honoured as written.
        assert!(!pattern_needs_prefix("src/*.rs"));
        assert!(!pattern_needs_prefix("main.rs"));
    }

    #[test]
    fn a_question_mark_matches_exactly_one_character() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(!glob_match("a?c", "abbc"));
        // Still one segment.
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn a_literal_pattern_matches_only_itself() {
        assert!(glob_match("Cargo.toml", "Cargo.toml"));
        assert!(!glob_match("Cargo.toml", "x/Cargo.toml"));
        assert!(!glob_match("Cargo.toml", "Cargo.lock"));
    }

    #[test]
    fn a_trailing_wildcard_matches_the_remaining_path() {
        assert!(glob_match("src/*", "src/anything"));
        assert!(glob_match("src/**", "src/a/b/c"));
        assert!(!glob_match("src/*", "src/a/b"));
    }
}

#[cfg(test)]
mod download_tests {
    use super::looks_like_download;

    #[test]
    fn fetching_commands_are_recognised() {
        for c in [
            "curl -fsSL https://example.com/x.tar.gz -o /tmp/x",
            "wget https://example.com/x",
            "git clone https://github.com/a/b",
            "git fetch origin",
            "git pull",
            "pip install requests",
            "pip3 install -r req.txt",
            "npm install",
            "cargo install ripgrep",
            "brew upgrade",
            "apt-get install -y jq",
            "docker pull ubuntu",
            "rustup update",
            "aria2c https://example.com/x",
            "scp host:/a /b",
        ] {
            assert!(looks_like_download(c), "should be a download: {c}");
        }
    }

    #[test]
    fn local_commands_are_not_mistaken_for_downloads() {
        // The cost of a miss here is only a longer leash, but a *false* positive on
        // ordinary work would change behaviour the user did not ask for -- and these are
        // the command lines most likely to be run all day.
        for c in [
            "ls -la",
            "cargo build --release",
            "cargo test",
            "cargo run",
            "git status",
            "git log --oneline -5",
            "git diff",
            "pip list",
            "npm ls",
            "npm run build",
            "grep -rn download .",
            "cat download.txt",
            "echo 'install later'",
            "docker ps",
            "rustup show",
        ] {
            assert!(!looks_like_download(c), "is not a download: {c}");
        }
    }

    #[test]
    fn a_tool_name_alone_does_not_decide_it() {
        // `npm` and `cargo` fetch for some verbs and not others, so the verb is what
        // matters -- flagging the tool would make every local build look like a transfer.
        assert!(looks_like_download("npm install"));
        assert!(!looks_like_download("npm run build"));
        assert!(looks_like_download("cargo install x"));
        assert!(!looks_like_download("cargo build"));
        assert!(looks_like_download("git clone x"));
        assert!(!looks_like_download("git commit -m x"));
    }
}

#[cfg(test)]
mod progress_parse_tests {
    use super::split_progress_lines;

    #[test]
    fn a_carriage_return_separates_progress_updates() {
        // A progress bar redraws one line by returning the carriage; without splitting on
        // `\r` the whole transfer stays in one buffer and no progress is ever reported.
        let chunk = b"\r  1% [=>          ]\r 45% [=====>     ]\r100% [===========]";
        assert_eq!(
            split_progress_lines(chunk),
            vec!["1% [=>          ]", "45% [=====>     ]", "100% [===========]"]
        );
    }

    #[test]
    fn ordinary_lines_still_split_on_newline() {
        let chunk = b"line one\nline two\n";
        assert_eq!(split_progress_lines(chunk), vec!["line one", "line two"]);
    }

    #[test]
    fn blank_and_whitespace_only_pieces_are_dropped() {
        // A redraw is often a row of spaces then a carriage return. Reporting that as the
        // command's activity would replace a useful line with nothing.
        let chunk = b"\n   \r\t\r\nreal line\n";
        assert_eq!(split_progress_lines(chunk), vec!["real line"]);
    }

    #[test]
    fn a_chunk_with_no_terminator_is_still_reported() {
        // The tail of the output usually arrives without one, and it is where the error
        // message lives.
        assert_eq!(split_progress_lines(b"last words"), vec!["last words"]);
    }
}

#[cfg(test)]
mod spill_tests {
    use super::*;
    use crate::config::Config;

    /// A directory of this test's own, removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flint-tools-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An answer over the budget keeps both ends, and the whole of it is on disk.
    ///
    /// The point is not that something was cut -- cutting is the old behaviour too -- but
    /// that the part which was cut can still be read, and that the reply says where. A
    /// build log whose last line is gone is a log whose error message is gone.
    #[tokio::test]
    async fn a_long_answer_is_spilled_and_named_in_the_reply() {
        let dir = TempDir::new("spill");
        let file = dir.path().join("log.txt");
        let whole: String = (0..300).map(|n| format!("line {n:03} of the log\n")).collect();
        std::fs::write(&file, &whole).unwrap();

        let config = Config {
            max_tool_output: 1000,
            ..Config::default()
        };
        let tools = ToolBox::new(&config, false, dir.path().to_path_buf())
            .with_spill_dir(dir.path().join("spill"));
        let out = tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read");

        assert!(out.contains("line 000 of the log"), "the head is gone: {out}");
        assert!(out.contains("line 299 of the log"), "the tail is gone: {out}");
        assert!(out.contains("full output:"), "the reply does not say where: {out}");

        // The spill holds what the tool actually returned, in full -- `read` numbers its
        // lines, so it is that text and not the raw file.
        let spilled = std::fs::read_to_string(dir.path().join("spill").join("1.txt"))
            .expect("the spill file must exist, since the reply points at it");
        assert!(spilled.contains("line 000 of the log"), "the spill is missing the start");
        assert!(spilled.contains("line 299 of the log"), "the spill is missing the end");
        assert!(
            spilled.chars().count() > out.chars().count(),
            "the spill is no longer than the reply, so the cut part is nowhere"
        );

        // The next one is numbered, not overwritten: two long answers in one session are
        // two files, so the first is still there when the second arrives.
        let out = tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read again");
        assert!(out.contains("2.txt"), "the second spill reused a name: {out}");
        assert!(dir.path().join("spill").join("2.txt").exists());
        assert!(dir.path().join("spill").join("1.txt").exists());
    }

    /// A tool answer inside the budget is returned untouched, with no note about a file.
    #[tokio::test]
    async fn a_short_answer_is_not_spilled() {
        let dir = TempDir::new("short");
        let file = dir.path().join("small.txt");
        std::fs::write(&file, "one line\n").unwrap();

        let config = Config {
            max_tool_output: 1000,
            ..Config::default()
        };
        let tools = ToolBox::new(&config, false, dir.path().to_path_buf())
            .with_spill_dir(dir.path().join("spill"));
        let out = tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read");

        assert!(out.contains("one line"));
        assert!(!out.contains("full output:"), "a note on nothing: {out}");
        assert!(
            !dir.path().join("spill").exists(),
            "a spill directory was made for output that fit"
        );
    }
}
