//! The tool set. Five tools, and that is the whole surface area.
//!
//! Permission model: full by default. There is exactly one switch — `readonly`
//! — exposed globally and per call, because in a rescue situation you want
//! "let it work" and "do not touch anything" and very little in between.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::Config;
use crate::context;
use crate::patch;
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
        // One record for the whole tool set: it is the run's memory of what it has looked
        // at, so every tool that reads has to write into the same one that the writers
        // consult.
        let reads = std::sync::Arc::new(Reads::default());
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(BashTool {
                config: config.clone(),
                readonly,
                cwd: cwd.clone(),
            }),
            Box::new(ExecTool {
                config: config.clone(),
                readonly,
                cwd: cwd.clone(),
            }),
            Box::new(ReadTool {
                cwd: cwd.clone(),
                reads: reads.clone(),
            }),
            Box::new(WriteTool {
                readonly,
                cwd: cwd.clone(),
                reads: reads.clone(),
            }),
            Box::new(EditTool {
                readonly,
                cwd: cwd.clone(),
                reads: reads.clone(),
            }),
            Box::new(PatchTool {
                readonly,
                cwd: cwd.clone(),
                reads: reads.clone(),
            }),
            Box::new(ListTool),
            Box::new(GlobTool { cwd: cwd.clone() }),
            Box::new(GrepTool { cwd: cwd.clone() }),
        ];
        // Always offered: reading a URL needs no credential, and this is the safe path to
        // the open web -- the alternative is `bash` and `curl`, which puts a page's raw
        // markup against the output budget and shows the model `<head>`. It is a boundary
        // around *this tool* and not around flint: `bash` reaches whatever the machine can.
        tools.push(Box::new(crate::fetch::FetchTool::new(config.proxy.clone())));
        // Offered only when it can actually search. A tool that always fails costs a schema
        // on every request and teaches the model that this tool is broken; the reason it is
        // missing is said once at startup instead (`search::Availability::Unavailable`).
        if let crate::search::Availability::Ready(backend) = crate::search::resolve(config) {
            tools.push(Box::new(crate::search::SearchTool::new(*backend)));
        }
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

/// The JSON name of a value's type, for an error that says what was actually sent.
///
/// "missing required string argument 'path'" is a lie when `path` was sent as a number,
/// and the lie costs a turn: the model reads the message, concludes it did pass the
/// argument, and sends the same call again.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

pub(crate) fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    match args.get(key) {
        Some(Value::String(text)) => Ok(text),
        Some(other) => Err(anyhow!(
            "argument '{key}' must be a string, but it is {}",
            type_name(other)
        )),
        None => Err(anyhow!("missing required string argument '{key}'")),
    }
}

/// An optional string argument, refusing a value of the wrong type.
///
/// The distinction matters in one direction only: absent is a choice the caller made,
/// whereas a value of the wrong type is an instruction that is about to be dropped. Read
/// as "absent", a mistyped `"path": 5` silently searches the working directory instead.
fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(other) => Err(anyhow!(
            "argument '{key}' must be a string, but it is {}",
            type_name(other)
        )),
    }
}

/// An optional whole-number argument, refusing a value of the wrong type.
fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("argument '{key}' must be a whole number, but it is {number}")),
        Some(other) => Err(anyhow!(
            "argument '{key}' must be a number, but it is {}",
            type_name(other)
        )),
    }
}

/// An optional boolean argument, refusing a value of the wrong type.
fn optional_bool(args: &Value, key: &str) -> Result<bool> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(other) => Err(anyhow!(
            "argument '{key}' must be true or false, but it is {}",
            type_name(other)
        )),
    }
}

/// An optional array of strings, refusing anything else, element by element.
///
/// The wrong shape here is not hypothetical: a model writing `"args": "commit -m x"` has
/// handed over one string where a list was wanted, and running it as a single argument
/// would be a silent, plausible-looking failure. So the refusal names the element that was
/// wrong, and says what the argument is for, because "must be an array" alone still leaves
/// the model guessing whether it should have been one string of several words.
fn optional_str_list(args: &Value, key: &str) -> Result<Vec<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                Value::String(text) => Ok(text.clone()),
                other => Err(anyhow!(
                    "argument '{key}' must be an array of strings, one entry per argument, \
                     but its element {i} is {}",
                    type_name(other)
                )),
            })
            .collect(),
        Some(other) => Err(anyhow!(
            "argument '{key}' must be an array of strings, one entry per argument -- \
             [\"commit\", \"-m\", \"message\"] -- but it is {}",
            type_name(other)
        )),
    }
}

/// What this run has read, so a write can tell whether it is overwriting something the
/// model has actually looked at.
///
/// In memory and never persisted. It is a fact about this run's attention, not about the
/// file: a copy on disk would be derived state that goes quietly wrong after a restart, and
/// the failure it causes -- a write allowed because *some earlier process* read the file --
/// is exactly the failure the gate exists to prevent.
///
/// The value is what the file looked like when it was read, not merely that it was, so a
/// file changed by something else in the meantime is caught too. That is the case a
/// read-tracking gate is usually built for and usually misses: the model read the file
/// three turns ago, a build regenerated it, and the edit that follows is aimed at text
/// that is no longer there.
#[derive(Default)]
pub struct Reads {
    seen: std::sync::Mutex<std::collections::HashMap<PathBuf, Fingerprint>>,
}

/// Enough of a file's identity to notice that it is no longer the file that was read.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    modified: Option<std::time::SystemTime>,
    len: u64,
}

impl Fingerprint {
    fn of(meta: &std::fs::Metadata) -> Self {
        Fingerprint {
            modified: meta.modified().ok(),
            len: meta.len(),
        }
    }
}

impl Reads {
    /// Record that `path` has been read, as it is now.
    fn note(&self, path: &Path) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.insert(path.to_path_buf(), Fingerprint::of(&meta));
    }

    /// Refuse to modify `path` unless this run has read it and it has not changed since.
    ///
    /// A file that is not there is always allowed: creating it destroys nothing, and
    /// demanding a read of a file that does not exist would make `write` unable to create
    /// anything at all.
    fn check(&self, path: &Path) -> Result<()> {
        let Ok(meta) = std::fs::metadata(path) else {
            return Ok(());
        };
        let now = Fingerprint::of(&meta);
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        match seen.get(path) {
            None => Err(anyhow!(
                "cannot modify \"{}\": file has not been read — read the file, then retry",
                path.display()
            )),
            Some(then) if *then != now => Err(anyhow!(
                "cannot modify \"{}\": file has changed on disk since it was read — read it again, then retry",
                path.display()
            )),
            Some(_) => Ok(()),
        }
    }
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

/// Run a shell command, reporting its output as it arrives.
///
/// The shell is only a *program plus arguments* here: this resolves which shell to use,
/// hands it the command string as its one argument, and everything after that is
/// [`run_program_streaming`]. `bash` is the special case, not the implementation.
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
    let (program, shell_args) = shell
        .split_first()
        .expect("probe_shell always names a program");
    let mut argv: Vec<String> = shell_args.to_vec();
    argv.push(command.to_string());
    // No context is wrapped around the result: `killed after 120s` does not need to be
    // prefixed with the shell that was running it, and a spawn that fails already names
    // the program it could not start.
    run_program_streaming(
        config,
        program,
        &argv,
        None,
        cwd,
        timeout_secs,
        idle_kill,
    )
    .await
}

/// What a person would call this command: the program, then its arguments.
///
/// A label and nothing else. It is never parsed back into arguments, never executed, and
/// never handed to a shell -- it exists for the one heuristic that has to read a command as
/// a line (`output_size_note`, which guesses the file a download is writing to). Deriving
/// it in a single place is what keeps that guess from quietly disagreeing with the argv
/// that actually runs.
///
/// The shape is approximate for `bash`, whose command string is one long argument: `sh -c
/// "curl -o f url"` labels as several words, and is still a string a heuristic can read.
/// Nothing downstream cares which form it came from.
fn label_of(program: &str, argv: &[String]) -> String {
    std::iter::once(program)
        .chain(argv.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run one program, with its arguments already separate, and report its output as it
/// arrives.
///
/// The arguments are handed to the child as an array and **never** concatenated into a
/// command line, here or by anything between. On Windows that is the whole difference
/// between an argument arriving and an argument being re-parsed by whichever runtime
/// happens to be in the middle; see `docs/windows-tooling.md` §1. This is the reason the
/// function exists, and the reason `exec` and `bash` share it rather than each having
/// their own runner: a fix to the timeout, the progress reporting, the spill or the kill
/// is a fix for every tool that runs a process.
///
/// The label is derived from the argv rather than passed in, and `stdin` is the one payload
/// that cannot be an argument: a document, a JSON body or a regex belongs on the child's
/// standard input, where nothing re-parses it on the way.
pub async fn run_program_streaming(
    config: &Config,
    program: &str,
    argv: &[String],
    stdin: Option<&str>,
    cwd: &Path,
    timeout_secs: u64,
    idle_kill: bool,
) -> Result<CommandOutcome> {
    let label = label_of(program, argv);
    let label = label.as_str();
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(argv);
    cmd.current_dir(cwd);
    // Only piped when there is something to write; `null` is the honest default, since a
    // program that waits for input on a terminal flint does not have would hang until the
    // timeout, and the error it gets instead says exactly that.
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
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
        .with_context(|| format!("cannot spawn program '{program}'"))?;

    // The caller's text goes in through the pipe, never through the command line.
    if let Some(text) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            let bytes = text.as_bytes().to_vec();
            // Detached, and not awaited: a program that never reads its input would
            // otherwise block a runner that has not started its clock yet, and the timeout
            // -- the one thing that would end the hang -- would never be armed. The task
            // ends when the write fails, and closing the handle is what gives the child its
            // EOF; a child that exits first simply makes the write fail.
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = pipe.write_all(&bytes).await;
                let _ = pipe.shutdown().await;
            });
        }
    }

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
                // No hint about a `[timeout:N]` marker in the command: nothing parses one.
                // Telling the model to write a syntax that does not exist costs it a turn
                // and teaches it that the tool's own messages cannot be believed. The
                // argument is the only way to raise the budget.
                return Err(anyhow!(
                    "killed after {timeout_secs}s with no result. Pass a larger timeout_secs if it \
                     is genuinely slow; if it is waiting for input, it never will -- stdin is closed."
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
                        output_size_note(label)
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
                         {timeout_secs}s; pass a larger `timeout_secs` if it is genuinely slow.",
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
        let declared = optional_bool(args, "download")?;
        let download = declared || looks_like_download(command);

        let timeout = match optional_u64(args, "timeout_secs")? {
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

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/// The `exec` tool: a program and its arguments, with no shell anywhere.
///
/// This exists because a command *line* is re-read by every layer between the model and
/// the program, and each layer's escaping is correct for itself and wrong for the next.
/// An array has no second reader. `docs/windows-tooling.md` §1 is the long version; the
/// short one is that `git commit -m "a \"quoted\" message"` is a string that survives one
/// parser and loses a character in the second, while `["commit", "-m", "a \"quoted\"
/// message"]` is a JSON array whose elements are simply the arguments.
///
/// Registered on every platform, not just Windows. The reasoning that makes it necessary
/// there makes it better here too: the model stops writing a command line and starts
/// writing a list, and `readonly` gets to judge a program and a verb rather than guess
/// where the words are.
pub struct ExecTool {
    config: Config,
    readonly: bool,
    cwd: PathBuf,
}

#[async_trait::async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Run one program with its arguments already separate, and return its combined \
         output. Use this instead of `bash` for anything that is not shell syntax: no \
         shell is involved, so every argument arrives exactly as written -- quotes, \
         spaces, backslashes, `$`, `%` and non-ASCII text are all just characters. \
         Shell features do not work here (`|`, `>`, `&&`, `$VAR`, globbing, `cd`); use \
         `bash` for those. There is no approval prompt and nothing here can answer one, \
         so a step that needs a password or an elevation prompt is the user's to run."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "program": {
                    "type": "string",
                    "description": "The program to run: `git`, `cargo`, `python3`. A bare name is searched on PATH."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "One string per argument, in order, exactly as the program should receive it. No quoting and no escaping: [\"commit\", \"-m\", \"a message with spaces\"] is three arguments. Put a regex, a JSON body or any other long or quote-heavy text in `stdin`, or in a file whose path you pass."
                },
                "stdin": {
                    "type": "string",
                    "description": "Text to write to the program's standard input."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": format!(
                        "Kill the program after this many seconds (default {DEFAULT_BASH_TIMEOUT} \
                         for ordinary commands, {LONG_BASH_TIMEOUT} when it installs, builds or \
                         downloads). Raise it for anything known to be slow."
                    )
                }
            },
            "required": ["program"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let program = require_str(args, "program")?;
        if program.trim().is_empty() {
            return Err(anyhow!("argument 'program' must name a program"));
        }
        let argv = optional_str_list(args, "args")?;
        let stdin = optional_str(args, "stdin")?;

        // The time and download heuristics read the call as a line, which is the only
        // place an argument array is ever flattened. Nothing is executed from it: the
        // runner below gets `program` and `argv` exactly as they arrived.
        let label = label_of(program, &argv);
        let download = looks_like_download(&label);

        let timeout = match optional_u64(args, "timeout_secs")? {
            Some(explicit) => explicit,
            None if download => DOWNLOAD_BASH_TIMEOUT,
            None if looks_slow(&label) => LONG_BASH_TIMEOUT,
            None => DEFAULT_BASH_TIMEOUT,
        }
        .clamp(1, 24 * 3600);

        if self.readonly && !exec_is_readonly(program, &argv) {
            return Err(anyhow!(
                "readonly mode is ON: refusing to run `{}`. \
                 Turn it off with /readonly, or run a program that only inspects.",
                util::preview(&label, 120)
            ));
        }

        let outcome = run_program_streaming(
            &self.config,
            program,
            &argv,
            stdin,
            &self.cwd,
            timeout,
            download,
        )
        .await?;
        Ok(util::truncate(&outcome.report, self.config.max_tool_output))
    }
}

/// Conservative read-only classifier for a command *line*.
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
///
/// The metacharacter half is about the *string*, and it is the half `exec` must not
/// inherit: see [`exec_is_readonly`].
pub fn is_readonly_command(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let lower = command.trim().to_ascii_lowercase();

    // Composition and privilege escalation always disqualify a command.
    for bad in [">", ">>", "&&", "||", ";", "|", "`", "$(", "sudo", "doas"] {
        if lower.contains(bad) {
            return false;
        }
    }

    let words: Vec<String> = lower.split_whitespace().map(str::to_string).collect();
    is_readonly_words(&words)
}

/// Whether a program and its already-separated words are inspection only.
///
/// Split out of the string form because `exec` hands over an array, and there the string
/// rules would be answering a question nobody asked: a `>` in an argument is a
/// greater-than sign rather than a redirect, and `&&` is two ampersands, because no shell
/// ever sees that argv. What is left is the judgment that was always the useful one --
/// which program this is, and for the ones that can both inspect and mutate, which verb.
///
/// The first word is expected lower-case and bare; a caller holding a path (`/usr/bin/git`,
/// `git.exe`) reduces it to a program name first.
fn is_readonly_words(words: &[String]) -> bool {
    let word = |i: usize| words.get(i).map(String::as_str);
    let Some(first) = word(0) else {
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
            return git_is_read_only(word(1));
        }
        return true;
    }

    let second = word(1).unwrap_or("");

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

/// Whether `exec` may run a program in readonly mode.
///
/// The judgement is made on the arguments as they were sent, not on a string that has been
/// joined and re-split: `git`, `["status"]` is inspection, and there is no way for a space
/// inside an argument to look like a second word and change the answer.
fn exec_is_readonly(program: &str, argv: &[String]) -> bool {
    let mut words = vec![program_stem(program)];
    words.extend(argv.iter().map(|a| a.to_ascii_lowercase()));
    is_readonly_words(&words)
}

/// The bare, lower-case name of a program, from whatever the caller wrote.
///
/// `git`, `/usr/bin/git` and `C:\Program Files\Git\cmd\git.exe` are the same program, and
/// the classifier only knows the first form.
fn program_stem(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| program.to_ascii_lowercase())
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
    /// Shared with the writers, which refuse to modify a file this run has not read.
    reads: std::sync::Arc<Reads>,
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
        let offset = optional_u64(args, "offset")?.unwrap_or(1).max(1) as usize;
        let limit = optional_u64(args, "limit")?.unwrap_or(2000) as usize;

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
        // Noted before the cap: what the gate needs is that the model has seen this file,
        // and a read shortened for the request still means it looked.
        self.reads.note(&path);
        Ok(util::truncate(&out, 60_000))
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

pub struct WriteTool {
    readonly: bool,
    cwd: PathBuf,
    reads: std::sync::Arc<Reads>,
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create or completely overwrite a text file. Parent directories are \
         created automatically. An existing file must have been read first, and is \
         refused if it has changed on disk since. For small changes prefer `edit`."
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
        // A write replaces the whole file, so whatever was there is gone. Refusing until
        // it has been read is the difference between "the model meant to replace this" and
        // "the model had a wrong idea of what this was".
        self.reads.check(&path)?;

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
        // The tool just wrote the exact contents, so it knows what is there; requiring a
        // read back would be a round trip to learn what this call already decided.
        self.reads.note(&path);
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
    reads: std::sync::Arc<Reads>,
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. `old_string` must match byte for byte \
         and, unless `replace_all` is true, must appear exactly once. The file must \
         have been read first."
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
        let replace_all = optional_bool(args, "replace_all")?;

        if old.is_empty() {
            return Err(anyhow!("old_string must not be empty"));
        }
        // The same gate as `write`. An edit is narrower, and its `old_string` would fail on
        // its own if the text had changed -- but "the text I meant to change is not here"
        // and "you never looked at this file" are different mistakes, and the second one
        // is worth naming before the model starts editing by trial and error.
        self.reads.check(&path)?;

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
        // As with `write`: the tool produced these exact contents, so the next edit does
        // not need a read to be allowed.
        self.reads.note(&path);
        Ok(format!(
            "edited {} ({count} replacement{})",
            path.display(),
            if count == 1 { "" } else { "s" }
        ))
    }
}

// ---------------------------------------------------------------------------
// apply_patch
// ---------------------------------------------------------------------------

pub struct PatchTool {
    readonly: bool,
    cwd: PathBuf,
    reads: std::sync::Arc<Reads>,
}

#[async_trait::async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a patch to one or more text files, all or nothing. Use this instead of \
         several `edit` calls when one change spans files. Format: `*** Begin Patch`, then \
         one section per file -- `*** Add File: <path>`, `*** Update File: <path>` or \
         `*** Delete File: <path>` -- ending with `*** End Patch`. In an update, a line \
         starting with `+` is added, `-` is removed, and a space is context; `@@ ... @@` \
         starts another hunk. Each hunk is found by its own lines, so a file that has \
         changed since you read it makes the whole patch fail instead of landing in the \
         wrong place. Files must have been read before being updated or deleted."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "The patch text, from `*** Begin Patch` to `*** End Patch`."
                }
            },
            "required": ["patch"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        if self.readonly {
            return Err(anyhow!(
                "readonly mode is ON: refusing to apply a patch. Turn it off with /readonly."
            ));
        }
        let changes = patch::parse(require_str(args, "patch")?)?;

        // Everything is worked out before anything is written. A patch is one edit: a
        // hunk that does not match in the third file must leave the first two as they
        // were, or the tree is left half-changed and the model has to work out which half.
        let mut planned: Vec<(PathBuf, Option<String>, &patch::Change)> = Vec::new();
        let mut touched: Vec<PathBuf> = Vec::new();
        for change in &changes {
            let path = resolve_path(&self.cwd, change.path());
            // Two sections for one file would both be planned from the contents on disk,
            // so the second would undo the first rather than build on it.
            if touched.contains(&path) {
                bail!(
                    "{} appears twice in one patch; put all of its changes in one section",
                    path.display()
                );
            }
            touched.push(path.clone());

            let current = match tokio::fs::read_to_string(&path).await {
                Ok(text) => Some(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(anyhow!(e))
                        .with_context(|| format!("cannot read {}", path.display()))
                }
            };
            // The same gate as `write` and `edit`: updating or deleting a file this run
            // has not read is refused, and so is one that changed since it was read.
            if !matches!(change, patch::Change::Add { .. }) {
                self.reads.check(&path)?;
            }
            let next = patch::apply_to(change, &path, current.as_deref())?;
            planned.push((path, next, change));
        }

        let mut summary = format!(
            "applied {} file change{}:\n",
            planned.len(),
            if planned.len() == 1 { "" } else { "s" }
        );
        for (path, next, change) in planned {
            match next {
                Some(content) => {
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
                    // This run produced these exact contents, so a following edit does not
                    // need a read first -- as with `write` and `edit`.
                    self.reads.note(&path);
                }
                None => {
                    tokio::fs::remove_file(&path)
                        .await
                        .with_context(|| format!("cannot delete {}", path.display()))?;
                }
            }
            summary.push_str(&format!("  {} {}\n", change.verb(), path.display()));
        }
        Ok(summary.trim_end().to_string())
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
        let raw = optional_str(args, "path")?.unwrap_or(".");
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

/// Translate a glob pattern's separators to the `/` that `glob_match` matches against.
///
/// `walk_files` builds every path with `/`, deliberately, so the same pattern means the
/// same thing on both platforms. A pattern written with `\` therefore matches nothing at
/// all -- and the answer it produces, "no files matching 'src\*.rs'", does not read as "your
/// pattern used the wrong separator"; it reads as "there is no such directory", which sends
/// the model looking for a different explanation of a tree it can see.
///
/// The model writes `\` on Windows because flint's own system prompt states that platform's
/// path separator. `\` is not an escape character in this glob dialect -- there is no escape
/// mechanism at all -- so translating it can only turn a pattern that matches nothing into
/// one that matches, never change which files an existing pattern finds.
///
/// Only *patterns* are translated. `grep`'s `pattern` is literal text to search for, where a
/// backslash is a character in a file rather than a separator, and a `path` goes to the
/// filesystem through `std::path`, which already accepts either separator on Windows and
/// must not have the meaning of a Unix filename rewritten on the way.
fn normalise_glob(pattern: &str) -> String {
    pattern.replace('\\', "/")
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
        let written = require_str(args, "pattern")?;
        // A pattern is matched against `/`-separated relative paths, so a Windows-style one
        // has to be translated first. The message at the end keeps what the caller wrote,
        // because that is the thing it has to correct.
        let pattern = normalise_glob(written);
        let raw = optional_str(args, "path")?.unwrap_or(".");
        let root = resolve_path(&self.cwd, raw);

        let files = walk_files(&root, WALK_LIMIT * 10);
        let mut hits: Vec<String> = files
            .into_iter()
            .filter(|p| {
                glob_match(&pattern, p)
                    || (pattern_needs_prefix(&pattern) && glob_match(&format!("**/{pattern}"), p))
            })
            .collect();
        hits.truncate(WALK_LIMIT);

        if hits.is_empty() {
            return Ok(format!("no files matching '{written}' under {}", root.display()));
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
        let raw = optional_str(args, "path")?.unwrap_or(".");
        let root = resolve_path(&self.cwd, raw);
        // A file filter is a glob against the same `/`-separated paths `walk_files`
        // produces, so it is translated the same way `glob`'s pattern is. `pattern` above
        // is *not*: there it is literal text to search for.
        let file_glob = optional_str(args, "glob")?.map(normalise_glob);
        let ignore_case = optional_bool(args, "ignore_case")?;

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
            if let Some(g) = &file_glob {
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

    /// A pattern written with the Windows separator finds the same files as a slash one.
    ///
    /// `walk_files` normalises every path to `/` and `glob_match` matches *those*, so
    /// `src\*.rs` matched nothing whatsoever -- and the result, "no files matching
    /// 'src\*.rs'", does not read as "your pattern used the wrong separator". It reads as
    /// "there is no such directory", which sends the model looking for a different
    /// explanation of a working tree it can see perfectly well.
    ///
    /// The model writes the backslash because flint's own prompt tells it the platform's
    /// path separator, which on Windows is `\`. So the fix belongs at the tool's door, not
    /// in `glob_match`: that function is handed a pattern by `glob` and a *path* by
    /// `grep`'s filter, and neither caller should have to know which it has.
    #[tokio::test]
    async fn a_backslash_pattern_finds_the_same_files_as_a_slash_pattern() {
        let dir = super::test_support::TempDir::new("glob-backslash");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "fn main() {}\n").unwrap();

        let tools = ToolBox::new(&Config::default(), false, dir.path().to_path_buf());
        let with_slash = tools
            .invoke("glob", &json!({ "pattern": "src/*.rs" }))
            .await
            .expect("glob");
        let with_backslash = tools
            .invoke("glob", &json!({ "pattern": r"src\*.rs" }))
            .await
            .expect("glob");
        assert!(with_slash.contains("src/main.rs"), "{with_slash}");
        assert_eq!(
            with_backslash, with_slash,
            "a Windows-style pattern must find the same files"
        );
    }

    /// And the same for `grep`'s file filter, which is a glob against the same `/`-separated
    /// paths -- while `grep`'s own `pattern` is *literal text to search for*, and must keep
    /// every backslash it was given.
    #[tokio::test]
    async fn grep_filters_by_a_backslash_glob_without_touching_the_search_text() {
        let dir = super::test_support::TempDir::new("grep-backslash");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "let x = a\\b;\n").unwrap();

        let tools = ToolBox::new(&Config::default(), false, dir.path().to_path_buf());
        let filtered = tools
            .invoke("grep", &json!({ "pattern": "a", "glob": r"src\*.rs" }))
            .await
            .expect("grep");
        assert!(filtered.contains("src/main.rs"), "{filtered}");

        // The backslash in the search text is a character in the file, not a separator.
        let literal = tools
            .invoke("grep", &json!({ "pattern": r"a\b" }))
            .await
            .expect("grep");
        assert!(
            literal.contains("src/main.rs"),
            "the search text must not be normalised: {literal}"
        );
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
mod test_support {
    use std::path::{Path, PathBuf};

    /// A directory of this test's own, removed when the test ends.
    pub(super) struct TempDir(PathBuf);

    impl TempDir {
        pub(super) fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "flint-tools-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            TempDir(dir)
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod spill_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;

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

#[cfg(test)]
mod gate_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn tools(dir: &Path) -> ToolBox {
        let config = Config::default();
        ToolBox::new(&config, false, dir.to_path_buf()).with_spill_dir(dir.join("spill"))
    }

    /// Overwriting a file nobody has read is refused, and the refusal says what to do.
    ///
    /// Without this the model can act on a guess: it has an idea of what a file contains,
    /// writes that idea over the real thing, and the real thing is gone. The check is that
    /// the file is untouched afterwards, not merely that an error came back.
    #[tokio::test]
    async fn a_write_to_an_unread_file_is_refused() {
        let dir = TempDir::new("gate-unread");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "what was here\n").unwrap();

        let err = tools(dir.path())
            .invoke(
                "write",
                &json!({ "path": file.to_str().unwrap(), "content": "something else\n" }),
            )
            .await
            .expect_err("an unread file must not be overwritten");

        let msg = format!("{err:#}");
        assert!(msg.contains("has not been read"), "{msg}");
        assert!(msg.contains("notes.txt"), "the refusal must name the file: {msg}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "what was here\n",
            "the file was overwritten anyway"
        );
    }

    /// Reading it first is what unblocks the write: the same call, one `read` earlier.
    #[tokio::test]
    async fn reading_first_allows_the_write() {
        let dir = TempDir::new("gate-read-first");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "what was here\n").unwrap();
        let tools = tools(dir.path());

        tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read");
        tools
            .invoke(
                "write",
                &json!({ "path": file.to_str().unwrap(), "content": "something else\n" }),
            )
            .await
            .expect("a file that has been read may be written");

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "something else\n");
    }

    /// A file that changed after it was read is refused too.
    ///
    /// This is the case a read-tracking gate usually misses: the model did read the file,
    /// three turns ago, and something else has written it since -- a build, a formatter, a
    /// generator. What it is about to overwrite is no longer what it saw.
    #[tokio::test]
    async fn a_file_changed_since_it_was_read_is_refused() {
        let dir = TempDir::new("gate-changed");
        let file = dir.path().join("generated.txt");
        std::fs::write(&file, "from the generator\n").unwrap();
        let tools = tools(dir.path());

        tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read");
        // Something else writes it, and to a different length, so no clock resolution is
        // being relied on to notice.
        std::fs::write(&file, "the generator ran again and wrote this instead\n").unwrap();

        let err = tools
            .invoke(
                "write",
                &json!({ "path": file.to_str().unwrap(), "content": "mine\n" }),
            )
            .await
            .expect_err("a file that changed under us must not be overwritten");
        let msg = format!("{err:#}");
        assert!(msg.contains("changed on disk"), "{msg}");
        assert!(msg.contains("generated.txt"), "{msg}");
    }

    /// Creating a file that is not there needs no read: nothing is being destroyed, and a
    /// gate that demanded one would make `write` unable to create anything.
    #[tokio::test]
    async fn a_new_file_can_be_written_without_reading() {
        let dir = TempDir::new("gate-new");
        let file = dir.path().join("nested").join("fresh.txt");

        tools(dir.path())
            .invoke(
                "write",
                &json!({ "path": file.to_str().unwrap(), "content": "hello\n" }),
            )
            .await
            .expect("a new file needs no read");

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
    }

    /// `edit` is behind the same gate, and says the same thing.
    #[tokio::test]
    async fn an_edit_to_an_unread_file_is_refused() {
        let dir = TempDir::new("gate-edit");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "keep this\n").unwrap();

        let err = tools(dir.path())
            .invoke(
                "edit",
                &json!({
                    "path": file.to_str().unwrap(),
                    "old_string": "keep",
                    "new_string": "lose"
                }),
            )
            .await
            .expect_err("an unread file must not be edited");
        assert!(format!("{err:#}").contains("has not been read"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep this\n");
    }

    /// A file this run wrote counts as known, so an edit straight after a write is allowed.
    ///
    /// The tool produced those exact bytes; making the model read them back would be a
    /// round trip to learn what the previous call already decided.
    #[tokio::test]
    async fn a_file_this_run_wrote_can_be_edited_without_reading_it() {
        let dir = TempDir::new("gate-wrote");
        let file = dir.path().join("notes.txt");
        let tools = tools(dir.path());

        tools
            .invoke(
                "write",
                &json!({ "path": file.to_str().unwrap(), "content": "one\n" }),
            )
            .await
            .expect("write");
        tools
            .invoke(
                "edit",
                &json!({
                    "path": file.to_str().unwrap(),
                    "old_string": "one",
                    "new_string": "two"
                }),
            )
            .await
            .expect("an edit after our own write");

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
    }
}

#[cfg(test)]
mod arg_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn tools(dir: &Path) -> ToolBox {
        ToolBox::new(&Config::default(), false, dir.to_path_buf())
    }

    /// A wrong type is named as a wrong type, not reported as missing.
    ///
    /// The message is what the model has to act on. "Missing required string argument
    /// 'path'" sends it to fix a problem it does not have: it did pass `path`, and the
    /// next turn is spent sending the same call again with the same type.
    #[tokio::test]
    async fn a_wrong_argument_type_is_reported_as_a_type_error() {
        let dir = TempDir::new("args-type");
        let err = tools(dir.path())
            .invoke("read", &json!({ "path": 5 }))
            .await
            .expect_err("a number is not a path");
        let msg = format!("{err:#}");
        assert!(msg.contains("'path' must be a string"), "{msg}");
        assert!(msg.contains("a number"), "it must say what it got: {msg}");
        assert!(
            !msg.contains("missing"),
            "an argument that was sent is not missing: {msg}"
        );
    }

    /// An optional number of the wrong type is refused instead of being ignored.
    ///
    /// Read as absent, `"limit": "5"` asks for five lines and gets two thousand, with
    /// nothing anywhere saying the argument was dropped.
    #[tokio::test]
    async fn an_optional_number_of_the_wrong_type_is_not_ignored() {
        let dir = TempDir::new("args-number");
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();

        let err = tools(dir.path())
            .invoke(
                "read",
                &json!({ "path": file.to_str().unwrap(), "limit": "5" }),
            )
            .await
            .expect_err("a string is not a limit");
        let msg = format!("{err:#}");
        assert!(msg.contains("'limit' must be a number"), "{msg}");
        assert!(msg.contains("a string"), "it must say what it got: {msg}");
    }

    /// The same for a boolean: a mistyped `replace_all` must not read as `false`.
    #[tokio::test]
    async fn an_optional_boolean_of_the_wrong_type_is_not_ignored() {
        let dir = TempDir::new("args-bool");
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x\n").unwrap();

        let err = tools(dir.path())
            .invoke(
                "edit",
                &json!({
                    "path": file.to_str().unwrap(),
                    "old_string": "x",
                    "new_string": "y",
                    "replace_all": "yes"
                }),
            )
            .await
            .expect_err("a string is not a boolean");
        assert!(
            format!("{err:#}").contains("'replace_all' must be true or false"),
            "{err:#}"
        );
    }

    /// Optional arguments that are simply absent still take their defaults.
    ///
    /// This is the other half of the check above: a validator that made every optional
    /// argument required would pass those tests and break every ordinary call. `list` is
    /// given an explicit path because it resolves relative ones against the process's
    /// working directory rather than the toolbox's -- which is the same directory in a real
    /// run, and not something a test in this process can change for itself.
    #[tokio::test]
    async fn absent_optional_arguments_keep_their_defaults() {
        let dir = TempDir::new("args-absent");
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        let tools = tools(dir.path());

        let out = tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read with no offset or limit");
        assert!(out.contains("one") && out.contains("two"), "{out}");

        let listed = tools
            .invoke("list", &json!({ "path": dir.path().to_str().unwrap() }))
            .await
            .expect("list");
        assert!(listed.contains("f.txt"), "{listed}");
    }
}

#[cfg(test)]
mod exec_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn toolbox(dir: &Path, readonly: bool) -> ToolBox {
        ToolBox::new(&Config::default(), readonly, dir.to_path_buf())
    }

    fn args_of(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The whole reason the tool exists: an argument arrives as it was written.
    ///
    /// `printf` is the child because it prints each argument verbatim, one per line for a
    /// `%s\n` format, and it is on every Unix. The arguments are the ones a command line
    /// loses -- a space, a double quote, a trailing backslash, a `$`, and non-ASCII text --
    /// and each must come back byte for byte. A tool that joined the array into a string
    /// and handed it to a shell fails this test on the second line.
    ///
    /// Deliberately not paired with a `cfg(windows)` half: the Windows equivalent needs a
    /// real Windows session, and a guessed assertion would be worse than an absent one.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_argument_reaches_the_program_exactly_as_written() {
        let dir = TempDir::new("exec-exact");
        let out = toolbox(dir.path(), false)
            .invoke(
                "exec",
                &json!({
                    "program": "printf",
                    "args": ["%s\n", "two words", "a \"quoted\" word", "C:\\dir\\", "$HOME", "中文 ok"]
                }),
            )
            .await
            .expect("printf should run");

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            vec!["two words", "a \"quoted\" word", "C:\\dir\\", "$HOME", "中文 ok"],
            "every argument must arrive exactly as written, one per line: {out:?}"
        );
    }

    /// `stdin` is how a payload that is not an argument gets in, and the child must see the
    /// end of it.
    ///
    /// The EOF matters as much as the bytes: a writer that left the pipe open would hang
    /// `cat` until the timeout, so getting this wrong fails the test on the clock rather
    /// than on the content.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_is_delivered_and_then_closed() {
        let dir = TempDir::new("exec-stdin");
        let out = toolbox(dir.path(), false)
            .invoke("exec", &json!({ "program": "cat", "stdin": "one\ntwo\n" }))
            .await
            .expect("cat should run");
        assert_eq!(out.trim(), "one\ntwo", "{out:?}");
    }

    /// A program that fails is a *result*, not an error, and the code must be visible.
    ///
    /// Exit 0 with nothing on stdout is the failure mode that costs the most: the model
    /// reads a silent success and builds the next step on a command that did nothing.
    #[tokio::test]
    async fn a_failing_program_reports_its_exit_code() {
        let dir = TempDir::new("exec-exit");
        let (program, args) = if cfg!(windows) {
            ("cmd", args_of(&["/C", "exit", "7"]))
        } else {
            ("sh", args_of(&["-c", "exit 7"]))
        };
        let out = toolbox(dir.path(), false)
            .invoke("exec", &json!({ "program": program, "args": args }))
            .await
            .expect("the program runs; its failure belongs in the result");
        assert!(out.contains("exit code: 7"), "{out:?}");
    }

    /// A command line where an argument list was wanted is refused.
    ///
    /// This is the one wrong shape that would otherwise *work*: the whole string becomes a
    /// single argument, the program receives `commit -m x` as one word, and whether that is
    /// an error depends on the program. Refusing it is the only way the model finds out.
    #[tokio::test]
    async fn a_command_line_is_not_accepted_as_an_argument_list() {
        let dir = TempDir::new("exec-shape");
        let err = toolbox(dir.path(), false)
            .invoke("exec", &json!({ "program": "git", "args": "commit -m x" }))
            .await
            .expect_err("a string is not a list");
        let msg = format!("{err:#}");
        assert!(msg.contains("must be an array of strings"), "{msg}");
        assert!(msg.contains("one entry per argument"), "say what was wanted: {msg}");
        assert!(msg.contains("a string"), "and say what arrived: {msg}");
    }

    /// A single bad element is named by position, not reported as a bad list.
    #[tokio::test]
    async fn a_non_string_element_is_named_by_position() {
        let dir = TempDir::new("exec-element");
        let err = toolbox(dir.path(), false)
            .invoke("exec", &json!({ "program": "git", "args": ["commit", 7] }))
            .await
            .expect_err("a number is not an argument");
        let msg = format!("{err:#}");
        assert!(msg.contains("element 1"), "{msg}");
        assert!(msg.contains("a number"), "{msg}");
    }

    /// In readonly mode, a program and its verb decide, not a string that was re-split.
    ///
    /// The `>>` case is why `exec` does not reuse `is_readonly_command`: in an argv those
    /// characters are text inside one argument, and a classifier reading a joined line has
    /// no way to know that, so it refuses the whole call. Both halves are asserted here,
    /// because the difference is the design decision rather than an implementation detail.
    #[test]
    fn readonly_reads_a_program_and_its_arguments() {
        assert!(exec_is_readonly("git", &args_of(&["status"])));
        assert!(exec_is_readonly("/usr/bin/git", &args_of(&["log", "--oneline"])));
        assert!(
            exec_is_readonly("git.exe", &args_of(&["status"])),
            "a Windows program name is the same program"
        );
        assert!(!exec_is_readonly("git", &args_of(&["commit", "-m", "x"])));
        assert!(!exec_is_readonly("git", &args_of(&[])), "no verb proves nothing");
        assert!(!exec_is_readonly("rm", &args_of(&["-rf", "/"])));
        assert!(!exec_is_readonly("npm", &args_of(&["install"])));
        assert!(!exec_is_readonly("cargo", &args_of(&["build"])));

        let log_with_a_redirect_in_a_pattern = args_of(&["log", "--grep=a>>b"]);
        assert!(exec_is_readonly("git", &log_with_a_redirect_in_a_pattern));
        assert!(!is_readonly_command("git log --grep=a>>b"));
    }

    /// The refusal happens before the program runs, and says which mode refused it.
    #[tokio::test]
    async fn readonly_refuses_a_mutating_program() {
        let dir = TempDir::new("exec-readonly");
        let err = toolbox(dir.path(), true)
            .invoke(
                "exec",
                &json!({ "program": "git", "args": ["commit", "-m", "x"] }),
            )
            .await
            .expect_err("readonly must refuse this");
        assert!(format!("{err:#}").contains("readonly mode is ON"), "{err:#}");
    }
}

#[cfg(test)]
mod patch_tool_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn tools(dir: &Path) -> ToolBox {
        ToolBox::new(&Config::default(), false, dir.to_path_buf())
    }

    /// One patch: a file created, a file changed, a file deleted.
    #[tokio::test]
    async fn one_patch_can_add_update_and_delete() {
        let dir = TempDir::new("patch-mixed");
        let keep = dir.path().join("keep.txt");
        let dead = dir.path().join("dead.txt");
        let new = dir.path().join("new.txt");
        std::fs::write(&keep, "before\n").unwrap();
        std::fs::write(&dead, "gone soon\n").unwrap();
        let tools = tools(dir.path());

        // Read what is about to change, which is what the gate asks of `edit` too.
        for path in [&keep, &dead] {
            tools
                .invoke("read", &json!({ "path": path.to_str().unwrap() }))
                .await
                .expect("read");
        }

        let text = format!(
            "*** Begin Patch\n\
             *** Add File: {new}\n\
             +hello\n\
             *** Update File: {keep}\n\
             -before\n\
             +after\n\
             *** Delete File: {dead}\n\
             *** End Patch\n",
            new = new.display(),
            keep = keep.display(),
            dead = dead.display(),
        );
        let out = tools
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect("apply");

        assert_eq!(std::fs::read_to_string(&new).unwrap(), "hello\n");
        assert_eq!(std::fs::read_to_string(&keep).unwrap(), "after\n");
        assert!(!dead.exists(), "the deleted file is still there");
        for verb in ["add", "update", "delete"] {
            assert!(out.contains(verb), "the summary omits {verb}: {out}");
        }
    }

    /// A patch that cannot be applied in full is not applied at all.
    ///
    /// This is the promise the tool exists for: several files in one patch are one edit.
    /// Half-applying leaves the tree in a state nobody asked for and the model working out
    /// which half landed.
    #[tokio::test]
    async fn a_bad_hunk_leaves_the_earlier_files_alone() {
        let dir = TempDir::new("patch-atomic");
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        let fresh = dir.path().join("fresh.txt");
        std::fs::write(&first, "one\n").unwrap();
        std::fs::write(&second, "two\n").unwrap();
        let tools = tools(dir.path());
        for path in [&first, &second] {
            tools
                .invoke("read", &json!({ "path": path.to_str().unwrap() }))
                .await
                .expect("read");
        }

        // The first section is fine, the second asks for text that is not in the file, and
        // a third would create a file.
        let text = format!(
            "*** Begin Patch\n\
             *** Update File: {first}\n\
             -one\n\
             +ONE\n\
             *** Update File: {second}\n\
             -not in this file\n\
             +whatever\n\
             *** Add File: {fresh}\n\
             +never written\n\
             *** End Patch\n",
            first = first.display(),
            second = second.display(),
            fresh = fresh.display(),
        );
        let err = tools
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("a patch that does not fit must fail");
        assert!(format!("{err:#}").contains("not in the file"), "{err:#}");

        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "one\n",
            "the first file was changed even though the patch failed"
        );
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two\n");
        assert!(!fresh.exists(), "a file was created by a patch that failed");
    }

    /// Updating or deleting a file this run has not read is refused, exactly as `edit` is.
    #[tokio::test]
    async fn an_update_to_an_unread_file_is_refused() {
        let dir = TempDir::new("patch-gate");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "keep me\n").unwrap();

        let text = format!(
            "*** Begin Patch\n\
             *** Update File: {}\n\
             -keep me\n\
             +lose me\n\
             *** End Patch\n",
            file.display()
        );
        let err = tools(dir.path())
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("an unread file must not be patched");
        assert!(format!("{err:#}").contains("has not been read"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me\n");
    }

    /// Adding over a file that is already there is refused rather than quietly replacing it.
    #[tokio::test]
    async fn adding_over_an_existing_file_is_refused() {
        let dir = TempDir::new("patch-add-exists");
        let file = dir.path().join("there.txt");
        std::fs::write(&file, "original\n").unwrap();

        let text = format!(
            "*** Begin Patch\n\
             *** Add File: {}\n\
             +replacement\n\
             *** End Patch\n",
            file.display()
        );
        let err = tools(dir.path())
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("adding an existing file must be refused");
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\n");
    }

    /// Two sections for the same file are refused: both would be planned from the contents
    /// on disk, so the second would undo the first instead of building on it.
    #[tokio::test]
    async fn two_sections_for_one_file_are_refused() {
        let dir = TempDir::new("patch-twice");
        let file = dir.path().join("twice.txt");
        std::fs::write(&file, "a\nb\n").unwrap();
        let tools = tools(dir.path());
        tools
            .invoke("read", &json!({ "path": file.to_str().unwrap() }))
            .await
            .expect("read");

        let text = format!(
            "*** Begin Patch\n\
             *** Update File: {path}\n\
             -a\n\
             +A\n\
             *** Update File: {path}\n\
             -b\n\
             +B\n\
             *** End Patch\n",
            path = file.display()
        );
        let err = tools
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("one file, two sections");
        assert!(format!("{err:#}").contains("appears twice"), "{err:#}");
    }

    /// Read-only mode refuses the tool before it looks at the patch at all.
    #[tokio::test]
    async fn readonly_refuses_a_patch() {
        let dir = TempDir::new("patch-readonly");
        let tools = ToolBox::new(&Config::default(), true, dir.path().to_path_buf());
        let text = "*** Begin Patch\n*** Add File: x\n+nope\n*** End Patch\n";
        let err = tools
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("read-only must refuse");
        assert!(format!("{err:#}").contains("readonly mode is ON"), "{err:#}");
        assert!(!dir.path().join("x").exists());
    }
}
