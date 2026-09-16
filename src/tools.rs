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

    /// Where this session keeps the files it writes, for a tool that writes one of its own.
    ///
    /// Most tools write nothing: their answer is text. `pwsh` writes the script it is about to
    /// run, and that script belongs beside the output it produced -- in the session's own
    /// directory, where a person can find it after reading the transcript. Told rather than
    /// asked for, because a tool is built before the session it belongs to names itself.
    fn use_spill_dir(&mut self, _dir: &Path) {}

    /// This tool as the `task` tool, for the one thing that has to be told later: which endpoint to
    /// hand a child. `None` for every other tool, which is what makes the caller a two-line loop
    /// rather than a downcast.
    fn as_task(&mut self) -> Option<&mut TaskTool> {
        None
    }
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
        // The session's own directory, until `with_spill_dir` names the real one. A tool
        // that writes a file of its own is built with it now rather than told later, so
        // that a `ToolBox` used without `with_spill_dir` still writes somewhere sane.
        let spill_dir = crate::config::spill_dir().join("unattached");
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
            // Always offered, and the endpoint is filled in by `with_task_endpoint` once the run
            // knows it. A `task` that cannot reach an endpoint fails with a classified cause rather
            // than mysteriously, which is worth one schema in the request.
            Box::new(TaskTool {
                cwd: cwd.clone(),
                readonly,
                provider: String::new(),
                model: String::new(),
            }),
        ];
        // Always offered: reading a URL needs no credential, and this is the safe path to
        // the open web -- the alternative is `bash` and `curl`, which puts a page's raw
        // markup against the output budget and shows the model `<head>`. It is a boundary
        // around *this tool* and not around flint: `bash` reaches whatever the machine can.
        tools.push(Box::new(crate::fetch::FetchTool::new(config.proxy.clone())));
        // Windows only, where `cmd` is the default shell and a serious management task wants
        // a script rather than a command line. See `PwshTool`.
        #[cfg(windows)]
        tools.push(Box::new(PwshTool::new(
            config,
            readonly,
            cwd.clone(),
            spill_dir.clone(),
        )));
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
            spill_dir,
            spilled: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// File this session's spill files under a name of their own.
    pub fn with_spill_dir(mut self, dir: PathBuf) -> Self {
        // Told to the tools as well as kept here: a tool that writes files of its own has to
        // write them where the transcript says they are.
        for tool in &mut self.tools {
            tool.use_spill_dir(&dir);
        }
        self.spill_dir = dir;
        self
    }

    /// Tell the `task` tool which endpoint it should hand to a child.
    ///
    /// Told rather than asked for, because the tool set is built before the run's provider is
    /// resolved (`Agent::new` has it; `ToolBox::new` does not), and told explicitly rather than left
    /// to the child's own default, because `--provider` and `--model` on this run's command line are
    /// nowhere in the config -- a child that resolved its own default would quietly be a different
    /// model than the one the caller is talking to.
    pub fn with_task_endpoint(mut self, provider: &str, model: &str) -> Self {
        for tool in &mut self.tools {
            if let Some(task) = tool.as_task() {
                task.provider = provider.to_string();
                task.model = model.to_string();
            }
        }
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

/// Why Win32 will not use this name literally, if it will not.
///
/// The *raw argument* rather than a resolved path, and that is deliberate: `Path::join` parses
/// `a:b.txt` as "file `b.txt` on drive A", so the resolved path has already lost the colon by
/// the time anything could notice it. What matters here is what the model wrote.
///
/// Measured through `std::fs` on this machine, because what happens is not what the
/// documentation implies. Every one of these returned `Ok` from `fs::write`:
///
///   * `NUL` -- and reading it back gave nothing. The data went to the null device, and
///     nothing in either return value says so.
///   * `trailing.` and `trailing ` -- and the file that appeared was `trailing`: a different
///     name, created without a word.
///   * `colon:stream.txt` -- and what appeared was an empty `colon`, with the data in an
///     alternate data stream that `dir` does not show and a read of the same path does.
///   * `q?`, `a|b`, `a<b`, `a>b` and `star*` are refused by the OS with error 123
///     (`InvalidFilename`), which is honest and needs nothing here.
///   * A 1619-character path worked (long paths are enabled on this machine), and a single
///     300-character *name* did not: 255 per component, and reported honestly.
///
/// The device names are refused as a family, even though this build created real files for
/// `CON`, `COM1`, `AUX`, `LPT1`, `PRN` and `NUL.txt`. Which of those is a device depends on
/// the Windows version and on how the path is opened, and a name that silently means "the
/// console" on one machine is not something to find out per machine. What is refused is the
/// exact bare name, because that is what was measured to be silent here.
///
/// A refusal rather than a rewrite: a `write` that reports success at a different path is
/// worse than one that fails, because the model has no way to notice.
#[cfg(windows)]
fn windows_name_problem(raw: &str) -> Option<String> {
    const DEVICES: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    // Both separators, because a model writes `a/b` and `a\b` and both reach Win32 the same way.
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    // `.` and `..` are not file names, and the OS refuses them honestly as directories. The
    // trailing-dot rule below would otherwise catch them and explain itself wrongly.
    if name == "." || name == ".." {
        return None;
    }
    if DEVICES.contains(&name.to_ascii_uppercase().as_str()) {
        return Some(format!(
            "`{name}` is a reserved device name on Windows: the write would go to the device \
             and still report success"
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        let shortened = name.trim_end_matches(['.', ' ']);
        return Some(format!(
            "`{name}` ends with a dot or a space, which Windows drops: this would write \
             `{shortened}` instead"
        ));
    }
    if name.contains(':') {
        return Some(format!(
            "`{name}` contains `:`, which Windows reads as an alternate data stream: the \
             data would not be in the file this names"
        ));
    }
    None
}

#[cfg(not(windows))]
fn windows_name_problem(_raw: &str) -> Option<String> {
    None
}

/// Refuse a path whose name Windows would silently rewrite, before anything is written.
fn refuse_unwritable_name(raw: &str) -> Result<()> {
    match windows_name_problem(raw) {
        Some(why) => Err(anyhow!("cannot write \"{raw}\": {why}")),
        None => Ok(()),
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

/// The file a `read`, `write` or `edit` call names.
///
/// `file_path` is what the schema asks for, because it is what a model reaches for when the tool is
/// shaped like the file tools it has used elsewhere; `path` is accepted as an alias, because it is
/// what every earlier version of flint asked for and a call that has worked thousands of times
/// should not start answering "missing required string argument".
///
/// Two names that disagree are refused rather than resolved. Picking one is this program deciding
/// which of two contradictory instructions was meant, and the wrong guess of that pair writes a file
/// where nobody asked -- which is the failure the read gate above exists to prevent, arriving
/// through the argument list instead.
fn require_path(args: &Value) -> Result<&str> {
    let named = args
        .get("file_path")
        .filter(|value| !value.is_null())
        .map(|_| require_str(args, "file_path"))
        .transpose()?;
    let older = args
        .get("path")
        .filter(|value| !value.is_null())
        .map(|_| require_str(args, "path"))
        .transpose()?;
    match (named, older) {
        (Some(named), Some(older)) if named != older => Err(anyhow!(
            "arguments 'file_path' and 'path' name different files ({named} and {older}); \
             they are the same argument and only one may be given"
        )),
        (Some(named), _) => Ok(named),
        (None, Some(older)) => Ok(older),
        (None, None) => Err(anyhow!("missing required string argument 'file_path'")),
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

/// A program to run, the arguments to run it with, and the one string that cannot be one.
///
/// `raw` is for a program that parses a command *line* rather than an argument list: it cannot
/// be handed the command as an ordinary argument, so it gets it here instead, appended
/// verbatim. Public because it is what [`run_program_streaming`] takes; both callers that run
/// a command string build it with `shell_invocation`, so the runner and the engine launcher
/// cannot disagree about `cmd`.
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub raw: Option<String>,
}

impl Invocation {
    /// A program and its arguments, with nothing that has to arrive verbatim.
    ///
    /// This is what `exec` is: no shell, and therefore no shell's quoting rules to satisfy.
    pub fn plain(program: &str, args: Vec<String>) -> Self {
        Invocation {
            program: program.to_string(),
            args,
            raw: None,
        }
    }
}

/// Point a command at what an [`Invocation`] says, including the argument that must not be
/// re-quoted.
///
/// One function because there would otherwise be three copies of the `raw_arg` dance -- the
/// runner, the engine launcher and the browser launcher -- and the whole point of `raw` is
/// that it is easy to get subtly wrong.
pub fn apply_invocation(cmd: &mut std::process::Command, run: &Invocation) {
    cmd.args(&run.args);
    #[cfg(windows)]
    if let Some(raw) = run.raw.as_deref() {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(raw);
    }
}

/// Decide how a command string reaches the shell.
///
/// `cmd.exe` is not a program that takes an argument list. It takes a command line, parsed by
/// its own rules, and std quotes arguments the C runtime's way — so a command containing a
/// double quote, which is most of them, reached cmd with a backslash in front of every quote,
/// and a quoted path failed outright. Measured on this machine, as an ordinary argument and
/// then through `/S /C` with the string appended verbatim:
///
/// | command | ordinary argument | `/S /C` and verbatim |
/// |---|---|---|
/// | `echo "hello"` | `\"hello\"` | `"hello"` |
/// | `dir /b "C:\Windows\System32\drivers\etc"` | "The filename, directory name, or volume label syntax is incorrect." | a listing |
/// | `echo one > "%TEMP%\f" && type "%TEMP%\f"` | the same error | `one` |
/// | `echo bad ^& rm` | `bad & rm` | `bad & rm` |
///
/// `/S` is the part that makes it exact: it says the quotes around the rest are the shell's
/// own delimiters, to strip them and use what is between them as written. Without it cmd
/// applies its quote-stripping heuristics and decides for itself where the command ends.
///
/// A POSIX shell needs none of this: `sh -c "the command"` takes the string as one argument,
/// and the kernel keeps it whole.
pub(crate) fn shell_invocation(config: &Config, command: &str) -> Invocation {
    let shell = probe_shell(&config.shell, &config.shell_args);
    let (program, shell_args) = shell.split_first().expect("probe_shell always names a program");
    let program = program.clone();
    let mut args: Vec<String> = shell_args.to_vec();

    #[cfg(windows)]
    if program_stem(&program) == "cmd" {
        // `/S` first: everything after `/C` is what it applies to.
        if !args.iter().any(|a| a.eq_ignore_ascii_case("/s")) {
            args.insert(0, "/S".to_string());
        }
        return Invocation {
            program,
            args,
            raw: Some(format!("\"{command}\"")),
        };
    }

    args.push(command.to_string());
    Invocation {
        program,
        args,
        raw: None,
    }
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
pub(crate) fn command_exists(program: &str) -> bool {
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
    let run = shell_invocation(config, command);
    // No context is wrapped around the result: `killed after 120s` does not need to be
    // prefixed with the shell that was running it, and a spawn that fails already names
    // the program it could not start.
    run_program_streaming(config, &run, None, cwd, timeout_secs, idle_kill).await
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
///
/// `run.raw` is a command string that must reach the program **verbatim**, with no quoting
/// added, for the one program that reads a command line instead of an argument list. It comes
/// from [`shell_invocation`], which is where the argument for it lives.
pub async fn run_program_streaming(
    config: &Config,
    run: &Invocation,
    stdin: Option<&str>,
    cwd: &Path,
    timeout_secs: u64,
    idle_kill: bool,
) -> Result<CommandOutcome> {
    let label = label_of(&run.program, &run.args);
    let label = label.as_str();
    let mut cmd = tokio::process::Command::new(&run.program);
    // Through the std command, because that is where the raw-argument extension lives; tokio's
    // is a wrapper around it and hands out the same handle.
    apply_invocation(cmd.as_std_mut(), run);
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
        .with_context(|| format!("cannot spawn program '{}'", run.program))?;
    // Armed while the command runs. On the abnormal paths below -- a timeout, an idle kill, or
    // a turn the user interrupted, which drops this whole future -- the shell is still alive
    // and this still has something to walk. Declared after `child` so that it drops *before*
    // it, which is the order the tree kill needs; see its own comment.
    let mut tree = KillTree::arm(child.id());

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

    // It ended on its own, so there is no tree left to end.
    tree.defuse();

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

/// Ends a command's whole process tree on Windows, when the command did not end on its own.
///
/// `kill_on_drop` terminates the process tokio spawned and nothing else, and `cmd.exe` has no
/// exec: it stays as the parent of whatever it was asked to run. So a timed-out `cargo build`
/// killed `cmd.exe` and left `cargo`/`rustc` running, holding `target/` and the release binary
/// -- the next build then fails with a lock error that has nothing to do with the code.
/// Measured on this machine: killing the shell leaves both `PING.EXE` and `conhost.exe` alive;
/// `taskkill /PID <pid> /T /F` ends the tree, and costs 358 ms against 345 ms for `/F` alone.
///
/// It has to run **before** the shell is gone, which is why this is a guard and not a cleanup
/// at the end: `/T` walks the shell's children, and once the shell has exited its children are
/// already reparented and `taskkill` reports "not found" (measured: exit 128) with the tree
/// still running. So the guard is defused the moment the command finishes by itself, and fires
/// only on the paths that kill it -- a timeout, an idle kill, or an interrupted turn dropping
/// the future this lives in.
///
/// On Unix this is nothing at all, and deliberately: `sh -c` usually *becomes* a lone command,
/// so the process killed is the command. A script that backgrounds work is the case where that
/// is not true, and it is not measured here -- see `docs/windows-tooling.md` §6.1.
struct KillTree {
    #[cfg(windows)]
    pid: Option<u32>,
}

impl KillTree {
    fn arm(pid: Option<u32>) -> Self {
        #[cfg(windows)]
        {
            KillTree { pid }
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            KillTree {}
        }
    }

    /// The command ended by itself; leave whatever it started alone.
    fn defuse(&mut self) {
        #[cfg(windows)]
        {
            self.pid = None;
        }
    }
}

impl Drop for KillTree {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(pid) = self.pid {
            // Blocking, and on purpose: this runs while the future is being dropped, so there
            // is no async context left to hand the wait to. It is the abnormal path only, and
            // the measured cost is under half a second.
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
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
    // `decode_child_text` rather than `from_utf8_lossy`, because a Windows console program
    // writes the console's code page: see its comment. Every path a child's output takes
    // goes through here, which is why the decode lives at this line and not in each tool.
    let text = util::decode_child_text(raw).trim().to_string();
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

/// The PowerShell on this machine, and what it says about itself.
///
/// Two facts, from one spawn. Which PowerShell it is matters because 5.1 and 7 do not parse the
/// same language -- a script written with `??` is a parse error on 5.1 and nothing says why --
/// and `$PSNativeCommandArgumentPassing` (7.3+) decides whether the arguments flint's model
/// writes for a native program are rewritten on the way, which is exactly the class of bug this
/// document exists for.
///
/// Asked once per process and cached, because the answer cannot change while flint runs and a
/// spawn is not free. Asked *synchronously*, because the system prompt is built off the async
/// path; the command is a one-liner under `-NoProfile -NonInteractive`, so the only way it can
/// fail to return is a broken installation, and the alternative -- not telling the model which
/// PowerShell it is talking to -- is a guess it cannot check.
#[cfg(windows)]
pub fn powershell() -> Option<&'static Powershell> {
    static FOUND: std::sync::OnceLock<Option<Powershell>> = std::sync::OnceLock::new();
    FOUND.get_or_init(probe_powershell).as_ref()
}

#[cfg(windows)]
pub struct Powershell {
    /// The program that answered: `pwsh` when it exists, `powershell` otherwise.
    pub program: String,
    /// Its version, as it reports it: `5.1.26100.6584`.
    pub version: String,
    /// How it passes arguments to native commands, or that the variable does not exist.
    pub native_args: String,
}

#[cfg(windows)]
fn probe_powershell() -> Option<Powershell> {
    // One statement, so that the version and the variable cannot come from two different
    // PowerShells. `Test-Path variable:` rather than `$null -eq`, because on 5.1 the variable
    // is not defined at all and reading it (even to compare) is the kind of thing that gets
    // stricter in later versions.
    let ask = "$PSVersionTable.PSVersion.ToString() + '|' + \
               $(if (Test-Path variable:PSNativeCommandArgumentPassing) \
                 { $PSNativeCommandArgumentPassing } else { 'not defined (before 7.3)' })";
    for program in ["pwsh", "powershell"] {
        if !command_exists(program) {
            continue;
        }
        let Ok(out) = std::process::Command::new(program)
            .args(["-NoProfile", "-NonInteractive", "-Command", ask])
            .stdin(Stdio::null())
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.trim().lines().next().unwrap_or("").trim();
        let Some((version, native_args)) = line.split_once('|') else {
            continue;
        };
        if version.is_empty() {
            continue;
        }
        return Some(Powershell {
            program: program.to_string(),
            version: version.trim().to_string(),
            native_args: native_args.trim().to_string(),
        });
    }
    None
}

/// One line for the system prompt's "Local facts", or nothing when there is no PowerShell.
///
/// The 5.1 warning is here rather than left to the model to derive: it is the single fact that
/// changes what to write next, and a model that has it does not spend a turn finding out.
#[cfg(windows)]
pub fn powershell_facts() -> Option<String> {
    let ps = powershell()?;
    let older = if ps.version.starts_with("5.") || ps.version.starts_with("4.") {
        ", so 7-only syntax (`??`, `?:`, `-Parallel`) will not parse"
    } else {
        ""
    };
    Some(format!(
        "{} {} — arguments to native commands: {}{older}",
        ps.program, ps.version, ps.native_args
    ))
}

// ---------------------------------------------------------------------------
// pwsh
// ---------------------------------------------------------------------------

/// The `pwsh` tool: a PowerShell script handed over as a file.
///
/// **Why a file**, since `-Command` was measured working: the script becomes an artifact. It
/// is a plain `.ps1` in flint's spill directory, so a person can read what ran, edit it and run
/// it again without flint, and the transcript can name the path. A command line is a string
/// that only exists inside a request; a file is state, and this repository's rule is that state
/// is hand-editable. `pwsh -Command` also has to fit in one command line (32k characters, and
/// less in practice), while a file does not.
///
/// What the *file* needs was measured on this machine, Windows PowerShell 5.1, and both
/// findings are requirements rather than hygiene:
///
///   * **A byte-order mark.** Without one, a script containing non-ASCII is read as the
///     machine's code page, silently: `Write-Output "你好"` printed `浣犲ソ` and exited 0. With
///     one it printed `你好`.
///   * **`-ExecutionPolicy Bypass`.** The effective policy here is `Restricted`, and `-File`
///     is then refused outright: "cannot be loaded because running scripts is disabled on this
///     system", exit 1. With `Bypass` the same script runs. It is passed on the command line
///     rather than left to the machine's configuration, because a tool that works only where
///     somebody has already loosened the policy is a tool that fails on the machine that needs
///     it.
///
/// `pwsh` is preferred and Windows PowerShell is the fallback, because both are PowerShell and
/// only one of them may be installed. Which one ran is named in the result: 5.1 and 7 differ in
/// their parsing and their output encoding, and a model debugging a script needs to know which
/// one produced what it is reading.
///
/// Registered on Windows only. Elsewhere `bash` is a better PowerShell than a PowerShell tool
/// would be, and `docs/windows-tooling.md` §2 argues that a tool should remove layers rather
/// than add them.
#[cfg(windows)]
pub struct PwshTool {
    config: Config,
    readonly: bool,
    cwd: PathBuf,
    /// Where the `.ps1` files go: this session's spill directory, told by `use_spill_dir`.
    script_dir: PathBuf,
    /// How many scripts this session has written, so each one gets its own name.
    scripts: std::sync::atomic::AtomicUsize,
}

#[cfg(windows)]
impl PwshTool {
    pub fn new(config: &Config, readonly: bool, cwd: PathBuf, script_dir: PathBuf) -> Self {
        PwshTool {
            config: config.clone(),
            readonly,
            cwd,
            script_dir,
            scripts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The PowerShell to run: the one the process has already found, or a fresh search.
    ///
    /// The same probe the system prompt uses, so a session asks once and both readers of the
    /// answer agree. The probe is the same decider as the tool itself: a PowerShell that
    /// cannot answer a question cannot run a script, so a `pwsh` that is installed but broken
    /// falls back to Windows PowerShell rather than failing every call.
    fn shell(&self) -> Option<&'static Powershell> {
        powershell()
    }

    /// Write the script where a person can find it, with the mark that makes it read as UTF-8.
    fn write_script(&self, script: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.script_dir)
            .with_context(|| format!("cannot create {}", self.script_dir.display()))?;
        let n = self
            .scripts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let path = self.script_dir.join(format!("script-{n}.ps1"));
        // The mark first, and the script byte for byte after it: a model's script is not
        // reformatted here, or the line numbers in an error message would not be its own.
        let mut bytes = Vec::with_capacity(script.len() + 3);
        bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        bytes.extend_from_slice(script.as_bytes());
        std::fs::write(&path, bytes)
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }
}

#[cfg(windows)]
#[async_trait::async_trait]
impl Tool for PwshTool {
    fn name(&self) -> &str {
        "pwsh"
    }

    /// The scripts go where the spilled output goes, so a session's directory holds the
    /// scripts it ran next to the output they produced.
    fn use_spill_dir(&mut self, dir: &Path) {
        self.script_dir = dir.to_path_buf();
    }

    fn description(&self) -> &str {
        "Run a PowerShell script on Windows and return its combined output. The script is \
         written to a `.ps1` file and run with `-File`, so multi-line scripts, `$args`, \
         functions and comments all behave as they do in a file, and the path in the result \
         is a real file that can be read and run again. Use this for Windows management \
         (services, registry, WMI/CIM, Hyper-V, event logs) where the equivalent command \
         line would be unreadable; use `exec` when one program with arguments is enough. \
         Refused in readonly mode: a script is arbitrary code and flint cannot judge one. \
         On Windows PowerShell 5.1 (the version is stated in your instructions) `>` and \
         `Out-File` write UTF-16LE, which the `read` tool will show as a file of NUL bytes: \
         write files with `Set-Content -Encoding utf8` instead."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "The PowerShell script, exactly as it should run. It is \
                         written to a file and passed to `-File`, so it is parsed once and \
                         needs no escaping for any outer shell."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments for the script, available in it as `$args`."
                },
                "stdin": {
                    "type": "string",
                    "description": "Text to write to the script's standard input, for a script \
                         that reads a pipeline."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": format!(
                        "Kill the script after this many seconds (default {DEFAULT_BASH_TIMEOUT}). \
                         Raise it for anything known to be slow."
                    )
                }
            },
            "required": ["script"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let script = require_str(args, "script")?;
        if script.trim().is_empty() {
            return Err(anyhow!("argument 'script' must not be empty"));
        }
        // Not judged, and not guessed at with the wrong vocabulary: `is_readonly_command` reads
        // a cmd or POSIX command line, and `Get-Process | Stop-Process` is not one. A refusal
        // that says so is honest; a classifier that answers about a language it does not know
        // would be worse than no answer.
        if self.readonly {
            return Err(anyhow!(
                "readonly mode is ON: refusing to run a PowerShell script. A script is \
                 arbitrary code and flint cannot judge one; use a read-only `bash` or `exec` \
                 command, or turn readonly off with /readonly."
            ));
        }
        let extra = optional_str_list(args, "args")?;
        let stdin = optional_str(args, "stdin")?;
        let timeout = optional_u64(args, "timeout_secs")?
            .unwrap_or(DEFAULT_BASH_TIMEOUT)
            .clamp(1, 24 * 3600);

        let shell = self.shell().ok_or_else(|| {
            anyhow!("no PowerShell on this machine: neither `pwsh` nor `powershell` could be run")
        })?;
        let script_path = self.write_script(script)?;

        let mut argv: Vec<String> = [
            "-NoProfile",
            "-NonInteractive",
            // See the type's comment: the policy on this machine refuses `-File` without it.
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        argv.push(script_path.display().to_string());
        argv.extend(extra);

        let run = Invocation::plain(&shell.program, argv);
        let outcome =
            run_program_streaming(&self.config, &run, stdin, &self.cwd, timeout, false).await?;

        // The version and the path first, so that a reader knows which PowerShell produced
        // this and where the script it ran still is. Both are needed to make sense of output
        // that looks wrong: 5.1 and 7 do not parse the same language.
        let report = format!(
            "{} ({}) -- script: {}\n{}",
            shell.version, shell.program, script_path.display(), outcome.report
        );
        Ok(util::truncate(&report, self.config.max_tool_output))
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

        // No shell, and therefore no shell's quoting rules to satisfy: `exec` hands over an
        // argument list. A caller that wants shell syntax wants `bash`, whose invocation is
        // built by `shell_invocation`.
        let run = Invocation::plain(program, argv);

        let outcome = run_program_streaming(&self.config, &run, stdin, &self.cwd, timeout, download)
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
                "file_path": { "type": "string", "description": "File path (absolute, or relative to the working directory). `path` is accepted as an alias." },
                "offset": { "type": "integer", "description": "1-based first line to return." },
                "limit": { "type": "integer", "description": "Maximum number of lines (default 2000)." }
            },
            "required": ["file_path"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let path = resolve_path(&self.cwd, require_path(args)?);
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
                "file_path": { "type": "string", "description": "File path to write. `path` is accepted as an alias." },
                "content": { "type": "string", "description": "Full file content." }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        if self.readonly {
            return Err(anyhow!(
                "readonly mode is ON: refusing to write files. Turn it off with /readonly."
            ));
        }
        let raw = require_path(args)?;
        // A name Windows would rewrite is refused before the read gate, because the gate
        // cannot see the file this would really write to. Checked on what was written, not on
        // the resolved path: `Path::join` turns `a:b.txt` into a path on drive A.
        refuse_unwritable_name(raw)?;
        let path = resolve_path(&self.cwd, raw);
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
        // Written in place, not to a temporary file renamed over the target. On Windows a
        // rename cannot replace a file another process holds open, so the usual atomic-write
        // trick fails for exactly the files somebody is most likely to have open. This is a
        // decision rather than an oversight; see `docs/windows-tooling.md` §6.5.
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
                "file_path": { "type": "string", "description": "File to edit. `path` is accepted as an alias." },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        if self.readonly {
            return Err(anyhow!(
                "readonly mode is ON: refusing to edit files. Turn it off with /readonly."
            ));
        }
        let raw = require_path(args)?;
        refuse_unwritable_name(raw)?;
        let path = resolve_path(&self.cwd, raw);
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
            // `read` shows the bytes as they are, so the model has seen the `\r` -- but the
            // text it writes back is written with `\n` out of habit, and a `\r` is invisible
            // in a transcript. Naming the line endings costs one sentence here and saves the
            // turn the model would otherwise spend guessing. Deliberately not normalised:
            // matching loosely would let an edit silently rewrite every line of the file,
            // which is a diff nobody asked for and the worst kind to review.
            let hint = if text.contains("\r\n") && old.contains('\n') && !old.contains("\r\n") {
                " This file uses CRLF line endings: put \r\n between lines in old_string."
            } else if !text.contains("\r\n") && old.contains("\r\n") {
                " This file uses LF line endings: old_string has \r\n in it, which is not there."
            } else {
                ""
            };
            return Err(anyhow!(
                "old_string not found in {}. Read the file first to get the exact text.{hint}",
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
            refuse_unwritable_name(change.path())?;
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

/// Ask another flint to do something, in its own context.
///
/// The same door a Python caller or an MCP client opens, opened from inside: it runs another flint
/// with `-p … --json`, reads that stream the way any caller does, and hands back the answer with the
/// facts a caller needs to judge it. Deliberately a process rather than a function call -- a child
/// that is a process has its death, its timeout, its output size and its exit code solved by the
/// operating system and by an interface that already exists, and a feature that worked from outside
/// but not from inside would be two behaviours to keep in step.
///
/// What it buys over "the model can already run `flint` through `bash`": the answer comes back with
/// the exit code, the outcome, the cause and the child's session path attached; the endpoint and
/// `readonly` are chosen by flint rather than typed into a command line by a model; `readonly` is
/// monotonic; and the depth is bounded, which `bash` cannot do.
///
/// Three things it deliberately is not. It is not a way to get more context: the child starts with no
/// history from here, so its value is isolation and least privilege, never a bigger window. It is not
/// free: the child is a whole run, and the tokens land on whoever pays for the endpoint. And it is not
/// a permission boundary: the child runs as the same user, with the same tools, and `readonly` is the
/// only switch either of them has.
pub struct TaskTool {
    /// Where the child works unless the call names another directory.
    cwd: PathBuf,
    /// Whether *this* run is readonly. A readonly run may not spawn a writing child: the flag is
    /// monotonic, or `readonly` would stop meaning anything the moment a model could call `task`.
    readonly: bool,
    /// The endpoint handed to the child, so that "same as here" stays true even when this run was
    /// started with `--provider`/`--model` flags that appear nowhere in the config.
    provider: String,
    model: String,
}

/// How deep a chain of flint runs may go.
///
/// Two, so a run may spawn a child and that child may spawn a grandchild. The point is not the number:
/// it is that a nested call is a spend that recurses, and nothing bounded it before this. Set in the
/// environment rather than by a flag, because a flag is something the model writes in the command line
/// it is composing, and a bound a model can edit is not a bound.
pub const MAX_TASK_DEPTH: u32 = 2;

/// How deep this run is. `FLINT_DEPTH` is written by one thing only: the `task` tool, for its child.
pub fn task_depth() -> u32 {
    std::env::var("FLINT_DEPTH")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// The child's command line, as a pure function so that its shape is held still by a test.
///
/// The endpoint is always passed explicitly. Left out, the child would resolve its own default --
/// which is right until this run was started with `--provider` or `--model`, at which point "another
/// flint like this one" would quietly mean a different model.
pub fn task_argv(
    exe: &Path,
    prompt: &str,
    cwd: &Path,
    readonly: bool,
    provider: &str,
    model: &str,
    schema: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![
        exe.display().to_string(),
        "-p".to_string(),
        prompt.to_string(),
        "--json".to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
    ];
    if readonly {
        argv.push("--readonly".to_string());
    }
    if !provider.is_empty() {
        argv.push("--provider".to_string());
        argv.push(provider.to_string());
    }
    if !model.is_empty() {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(path) = schema {
        argv.push("--schema".to_string());
        argv.push(path.display().to_string());
    }
    argv
}

/// What an exit code means, in the child's own vocabulary. Duplicated in
/// `examples/mcp/flint_server.py` because that one is Python: two doors, one set of words.
fn task_exit_meaning(code: i32) -> &'static str {
    match code {
        0 => "finished",
        1 => "failed, cause not classified",
        2 => "the arguments were wrong, so nothing was asked",
        65 => "the answer is not usable: a schema never matched, or the run ran out of steps",
        69 => "a person must act: no key, rejected credentials, or an account with no balance",
        75 => "the retries ran out; asking again later is right",
        130 => "the run was stopped",
        _ => "unknown",
    }
}

#[async_trait::async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Ask another flint to do a job in its own context and give back its answer. Use it for \
         work that is large or self-contained -- a wide search, reading a lot of files, a question \
         whose transcript you do not want in this conversation. The other flint starts fresh: it \
         cannot see this conversation, so the prompt has to stand alone. It is a whole model run, \
         so it costs what a run costs, and this tool waits for it. `readonly` here forces it there."
    }

    fn as_task(&mut self) -> Option<&mut TaskTool> {
        Some(self)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "What to ask, complete enough to stand alone: the other flint has no history from here."
                },
                "cwd": {
                    "type": "string",
                    "description": "Directory for the other flint to work in (default: this run's directory)."
                },
                "readonly": {
                    "type": "boolean",
                    "description": "Refuse writes and mutating commands there. Always true when this run is readonly."
                },
                "provider": { "type": "string", "description": "Another provider for this child (default: this run's)." },
                "model": { "type": "string", "description": "Another model for this child (default: this run's)." },
                "schema": {
                    "type": "object",
                    "description": "A JSON Schema for the answer; the validated object comes back beside the prose."
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "Stop the child after this long (default 600). Stopping is `/stop`, so half an answer survives."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let prompt = require_str(args, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(anyhow!("task requires a non-empty `prompt`"));
        }

        // Refused rather than truncated: a run that silently stopped descending and answered anyway
        // would be telling the model its child had done something it never did.
        let depth = task_depth();
        if depth + 1 > MAX_TASK_DEPTH {
            return Ok(format!(
                "not started: this is already a flint run at depth {depth}, and flint does not go \
                 deeper than {MAX_TASK_DEPTH}. Do this work here, or ask for it in a run that was \
                 started at the top."
            ));
        }

        let cwd = match optional_str(args, "cwd")? {
            Some(dir) => {
                let path = resolve_path(&self.cwd, dir);
                if !path.is_dir() {
                    return Err(anyhow!("task cwd is not a directory: {}", path.display()));
                }
                path
            }
            None => self.cwd.clone(),
        };
        // An absent `readonly` is false, which is what the schema promises.
        let requested_readonly = optional_bool(args, "readonly")?;
        // Monotonic: a readonly run cannot be talked into a writing child by its own model.
        let readonly = self.readonly || requested_readonly;
        let provider = optional_str(args, "provider")?
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.provider.clone());
        let model = optional_str(args, "model")?
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.model.clone());
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|secs| *secs > 0)
            .unwrap_or(600);

        // Through a file, for the same reason the Python caller does it: a schema inline has to
        // survive a command line, and a path always does.
        let schema_file = match args.get("schema") {
            Some(schema) if schema.is_object() => {
                let path = std::env::temp_dir().join(format!(
                    "flint-task-schema-{}-{}.json",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ));
                std::fs::write(&path, serde_json::to_string(schema)?)
                    .with_context(|| format!("writing {}", path.display()))?;
                Some(path)
            }
            Some(other) => {
                return Err(anyhow!(
                    "task `schema` must be a JSON Schema object, not {}",
                    other
                ))
            }
            None => None,
        };

        let exe = match std::env::var("FLINT_BIN") {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
            // The same binary as this one: a child built from anything else would be a different
            // flint than the one the caller is talking to. `FLINT_BIN` is for a wrapper that wants
            // to point at a specific build, and for the tests, which live outside the binary.
            _ => std::env::current_exe().context("cannot find the flint binary to run")?,
        };
        let argv = task_argv(
            &exe,
            prompt,
            &cwd,
            readonly,
            &provider,
            &model,
            schema_file.as_deref(),
        );

        let started = std::time::Instant::now();
        let outcome = self.run_child(&argv, depth + 1, timeout_secs).await;
        if let Some(path) = &schema_file {
            let _ = std::fs::remove_file(path);
        }
        let (mut text, session) = outcome?;
        text.push_str(&format!(
            "\n\ndepth: {} (this run is {depth}), readonly: {}, waited: {:.0}s",
            depth + 1,
            readonly,
            started.elapsed().as_secs_f64()
        ));
        if let Some(session) = session {
            text.push_str(&format!("\nsession: {session}"));
        }
        Ok(text)
    }
}

impl TaskTool {
    /// Run the child, read its stream, and turn it into the one piece of text the model gets back.
    ///
    /// The stream is read as it arrives rather than through `output()`, for one reason: a child that
    /// has to be stopped should be stopped the way a person stops one -- `/stop` on its stdin, which
    /// leaves the half-answer it had drawn in its session file -- and killing it is the fallback, not
    /// the first move. The answer is formatted answer-first, then the facts a caller needs to decide
    /// what to do with it, so a model that reads only the top still gets the reason.
    async fn run_child(
        &self,
        argv: &[String],
        depth: u32,
        timeout_secs: u64,
    ) -> Result<(String, Option<String>)> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};

        let mut child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("FLINT_DEPTH", depth.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("running {}", argv[0]))?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let reader = tokio::spawn(async move {
            let lines = tokio::io::BufReader::new(stdout).lines();
            let mut collected = Collected::default();
            let mut lines = Box::pin(lines);
            while let Ok(Some(line)) = lines.next_line().await {
                collected.absorb(&line);
            }
            collected
        });
        let complaints = tokio::spawn(async move {
            let mut text = String::new();
            let mut stderr = stderr;
            let _ = stderr.read_to_string(&mut text).await;
            text
        });

        let stopped = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait(),
        )
        .await;
        let mut note = String::new();
        let status = match stopped {
            Ok(status) => status?,
            Err(_) => {
                // Graceful first. `/stop` is the word the terminal takes, and the child keeps what it
                // had drawn; `kill` is only for a child that ignores it.
                if let Some(stdin) = child.stdin.as_mut() {
                    use tokio::io::AsyncWriteExt;
                    let _ = stdin.write_all(b"/stop\n").await;
                    let _ = stdin.flush().await;
                }
                match tokio::time::timeout(std::time::Duration::from_secs(20), child.wait()).await {
                    Ok(status) => {
                        note = format!("\nstopped after {timeout_secs}s: the child was told to stop\n");
                        status?
                    }
                    Err(_) => {
                        let _ = child.kill().await;
                        note = format!("\nkilled after {timeout_secs}s: the child did not stop when asked\n");
                        child.wait().await?
                    }
                }
            }
        };

        let collected = reader.await.unwrap_or_default();
        let stderr_text = complaints.await.unwrap_or_default();
        let code = status.code().unwrap_or(-1);

        let mut text = match collected.answer() {
            Some(answer) => answer,
            // Nothing on the stream at all: the reason is on stderr, and passing it on is the
            // difference between "the child failed" and "the child refused before it started".
            None => stderr_text.trim().to_string(),
        };
        if text.trim().is_empty() {
            text = "(the child said nothing)".to_string();
        }
        text.push_str(&format!(
            "\n\nexit code: {code} ({})",
            task_exit_meaning(code)
        ));
        if let Some(outcome) = &collected.outcome {
            text.push_str(&format!("\noutcome: {outcome}"));
        }
        if let Some(cause) = &collected.error_code {
            text.push_str(&format!("\ncause: {cause}"));
        }
        if let Some(error) = &collected.error {
            let first = error.lines().next().unwrap_or("").trim();
            text.push_str(&format!("\nerror: {first}"));
        }
        if let Some(result) = &collected.result {
            // The validated object, on one line: this is the half of "structured" that a shell
            // pipeline cannot give back.
            text.push_str(&format!("\nresult: {result}"));
        }
        for warning in collected.warnings.iter().take(3) {
            text.push_str(&format!("\nwarning: {warning}"));
        }
        text.push_str(&note);
        Ok((text, collected.session))
    }
}

/// The frames worth keeping from a child's stream, gathered in one place.
#[derive(Default)]
struct Collected {
    deltas: Vec<String>,
    messages: Vec<String>,
    result: Option<serde_json::Value>,
    error: Option<String>,
    error_code: Option<String>,
    outcome: Option<String>,
    session: Option<String>,
    warnings: Vec<String>,
}

impl Collected {
    /// Take one line of a child's `--json` stream. A line that is not JSON is kept as a warning
    /// rather than dropped: the child promises one object per line, so damage is worth passing on.
    fn absorb(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(_) => {
                self.warnings
                    .push(format!("unreadable line: {}", util::truncate(line, 120)));
                return;
            }
        };
        let text = |key: &str| event.get(key).and_then(|v| v.as_str()).map(str::to_string);
        match event.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "message.delta" => {
                if let Some(delta) = text("text") {
                    self.deltas.push(delta);
                }
            }
            "message.completed" => {
                if let Some(message) = text("text") {
                    self.messages.push(message);
                }
            }
            "result" => self.result = event.get("json").cloned(),
            "error" => {
                self.error = text("message");
                self.error_code = text("code");
            }
            "turn.completed" => self.outcome = text("outcome"),
            "session.started" => self.session = text("session"),
            "warning" => {
                if let Some(message) = text("message") {
                    self.warnings.push(message);
                }
            }
            _ => {}
        }
    }

    /// What the child said, in order. The deltas are the whole turn -- prose before a tool call
    /// included -- and `message.completed` is the fallback for a provider that streams none.
    fn answer(&self) -> Option<String> {
        let text = if self.deltas.is_empty() {
            self.messages.join("\n")
        } else {
            self.deltas.concat()
        };
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

#[cfg(test)]
mod task_tests {
    use super::*;
    use std::path::Path;

    /// The child's command line is the whole interface between two flints, so its shape is held
    /// still here rather than only observed end to end.
    #[test]
    fn the_child_command_line_says_exactly_what_the_child_should_be() {
        let exe = Path::new("/usr/local/bin/flint");
        let cwd = Path::new("/work");
        let plain = task_argv(exe, "look around", cwd, false, "deepseek", "deepseek-chat", None);
        assert_eq!(
            plain,
            vec![
                "/usr/local/bin/flint",
                "-p",
                "look around",
                "--json",
                "--cwd",
                "/work",
                "--provider",
                "deepseek",
                "--model",
                "deepseek-chat",
            ]
        );
        // One argument, not a command line: the prompt reaches the child as the bytes it is, which
        // is the reason this is argv and not a string handed to a shell.
        assert_eq!(plain[2], "look around");

        let guarded = task_argv(
            exe,
            "p",
            cwd,
            true,
            "p",
            "m",
            Some(Path::new("/tmp/s.json")),
        );
        assert!(guarded.contains(&"--readonly".to_string()));
        assert_eq!(guarded.last().unwrap(), "/tmp/s.json");

        // An endpoint flint does not know is left unspoken rather than passed as an empty string,
        // which the child would take as a provider named "".
        let bare = task_argv(exe, "p", cwd, false, "", "", None);
        assert!(!bare.iter().any(|a| a == "--provider"));
        assert!(!bare.iter().any(|a| a == "--model"));
        assert!(!bare.iter().any(|a| a == "--readonly"));
    }

    #[test]
    fn a_childs_stream_is_read_the_way_a_caller_reads_it() {
        let mut collected = Collected::default();
        for line in [
            r#"{"type":"session.started","session":"C:\\flint\\sessions\\x.jsonl"}"#,
            r#"{"type":"message.delta","text":"the answer "}"#,
            r#"{"type":"message.delta","text":"is 42"}"#,
            r#"not json at all"#,
            r#"{"type":"turn.completed","outcome":"complete"}"#,
            r#"{"type":"error","code":"insufficient_balance","message":"no money"}"#,
            r#"{"type":"result","json":{"ok":true}}"#,
        ] {
            collected.absorb(line);
        }
        // Deltas are the answer, in order and joined: a caller that got only the last frame would
        // lose the sentence before a tool call.
        assert_eq!(collected.answer().as_deref(), Some("the answer is 42"));
        assert_eq!(
            collected.session.as_deref(),
            Some(r"C:\flint\sessions\x.jsonl")
        );
        assert_eq!(collected.outcome.as_deref(), Some("complete"));
        assert_eq!(collected.error_code.as_deref(), Some("insufficient_balance"));
        assert_eq!(collected.result, Some(json!({"ok": true})));
        // A line that is not JSON is reported, not dropped: the child promises one object per line.
        assert_eq!(collected.warnings.len(), 1);
        assert!(collected.warnings[0].contains("unreadable line"));

        // No deltas at all: the completed message is the fallback, for a provider that streams none.
        let mut quiet = Collected::default();
        quiet.absorb(r#"{"type":"message.completed","text":"said once"}"#);
        assert_eq!(quiet.answer().as_deref(), Some("said once"));
        let empty = Collected::default();
        assert_eq!(empty.answer(), None);
    }

    /// The codes a caller branches on. If one of these changes meaning, a caller's branch is wrong.
    #[test]
    fn an_exit_code_is_translated_into_the_words_a_caller_branches_on() {
        assert_eq!(task_exit_meaning(0), "finished");
        assert!(task_exit_meaning(65).contains("not usable"));
        assert!(task_exit_meaning(69).contains("a person must act"));
        assert!(task_exit_meaning(75).contains("again later"));
        assert!(task_exit_meaning(130).contains("stopped"));
        assert_eq!(task_exit_meaning(3), "unknown");
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
mod invocation_tests {
    use super::*;
    use crate::config::Config;

    /// A command string reaches the shell in one piece, and `cmd` gets it on its own terms.
    ///
    /// This is the decision that `bash` on Windows lives or dies by: std quotes an argument the
    /// C runtime's way, and `cmd` reads a command line by its own rules, so a command with a
    /// quote in it arrived with a backslash in front of every one. Asserted per platform,
    /// because the difference between the two is the whole point of the function.
    #[test]
    fn a_command_string_is_shaped_for_the_shell_that_reads_it() {
        let config = Config {
            shell: if cfg!(windows) { "cmd" } else { "sh" }.to_string(),
            shell_args: if cfg!(windows) {
                vec!["/C".to_string()]
            } else {
                vec!["-c".to_string()]
            },
            ..Config::default()
        };

        let run = shell_invocation(&config, "echo \"hello\"");
        assert_eq!(run.program.to_ascii_lowercase().contains("cmd"), cfg!(windows));

        if cfg!(windows) {
            assert_eq!(
                run.args,
                vec!["/S".to_string(), "/C".to_string()],
                "cmd needs /S to strip the quotes around the string itself"
            );
            assert_eq!(
                run.raw.as_deref(),
                Some("\"echo \"hello\"\""),
                "the string must be handed over verbatim, wrapped once for /S"
            );
            assert!(
                !run.args.iter().any(|a| a.contains("hello")),
                "the command must not also be passed as a normal argument"
            );
        } else {
            assert_eq!(run.args, vec!["-c".to_string(), "echo \"hello\"".to_string()]);
            assert_eq!(run.raw, None, "only cmd needs the verbatim form");
        }
    }

    /// A shell named with `/S` already, or by its full path, is still cmd.
    #[cfg(windows)]
    #[test]
    fn the_cmd_check_reads_a_path_and_does_not_double_the_flag() {
        let config = Config {
            shell: r"C:\Windows\System32\cmd.exe".to_string(),
            shell_args: vec!["/S".to_string(), "/C".to_string()],
            ..Config::default()
        };
        let run = shell_invocation(&config, "dir");
        assert_eq!(run.args, vec!["/S".to_string(), "/C".to_string()]);
        assert_eq!(run.raw.as_deref(), Some("\"dir\""));
    }
}

#[cfg(test)]
mod name_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;

    /// The names Win32 silently rewrites are refused, and the ones it writes literally are not.
    ///
    /// Both halves matter. `NUL.txt`, `con.txt` and `trailing .txt` were measured creating real
    /// files on this machine, and refusing those would be a tool that cannot write a file it can
    /// write -- the same class of mistake in the other direction.
    #[cfg(windows)]
    #[test]
    fn a_name_windows_would_rewrite_is_refused() {
        for name in [
            "NUL",
            "nul",
            "CON",
            "com1",
            "LPT9",
            "trailing.",
            "trailing ",
            "a:b.txt",
            "dir\\NUL",
            "dir/sub/trailing.",
        ] {
            assert!(
                windows_name_problem(name).is_some(),
                "`{name}` must be refused rather than silently rewritten"
            );
        }
        for name in [
            "NUL.txt",
            "con.txt",
            "trailing .txt",
            "normal.txt",
            "a-b.txt",
            "..",
            "dir/sub/normal.txt",
        ] {
            assert!(
                windows_name_problem(name).is_none(),
                "`{name}` is a file Windows writes literally"
            );
        }
    }

    /// A file can be named by either spelling, and naming two of them is refused.
    ///
    /// `file_path` is what these three tools ask for now, because it is what a model reaches for
    /// when the tool is shaped like every other file tool it has used; `path` still works, because
    /// it is what every earlier version of flint asked for, and a call that has worked thousands of
    /// times should not start answering "missing required string argument". The rename is in the
    /// schema, where a model reads it, and the alias is in the reading, where an older caller needs
    /// it -- so this test asks both spellings the same question and compares the answers.
    ///
    /// Two names that disagree are refused rather than resolved: picking one would be this program
    /// deciding which of two contradictory instructions was meant, and the wrong guess of that pair
    /// is a file written where nobody asked.
    #[tokio::test]
    async fn a_file_can_be_named_by_either_spelling() {
        let dir = TempDir::new("both-names");
        let config = Config {
            max_tool_output: 10_000,
            ..Config::default()
        };
        let tools = ToolBox::new(&config, false, dir.path().to_path_buf());
        std::fs::write(dir.path().join("note.txt"), "one\ntwo\n").unwrap();

        let now = tools
            .invoke("read", &json!({ "file_path": "note.txt" }))
            .await
            .expect("`file_path` is the name the schema asks for");
        let before = tools
            .invoke("read", &json!({ "path": "note.txt" }))
            .await
            .expect("`path` is the name every earlier version asked for");
        assert_eq!(now, before, "the two spellings must find the same file");

        // The writing tools take it too -- and the read gate is on the file, not on the spelling.
        let written = tools
            .invoke(
                "write",
                &json!({ "file_path": "note.txt", "content": "one\ntwo\nthree\n" }),
            )
            .await
            .expect("write by file_path");
        assert!(written.contains("note.txt"), "{written}");
        let edited = tools
            .invoke(
                "edit",
                &json!({ "file_path": "note.txt", "old_string": "three", "new_string": "four" }),
            )
            .await
            .expect("edit by file_path");
        assert!(edited.contains("note.txt"), "{edited}");

        let conflict = tools
            .invoke(
                "read",
                &json!({ "path": "note.txt", "file_path": "other.txt" }),
            )
            .await
            .expect_err("two names that disagree must be refused");
        let message = conflict.to_string();
        assert!(
            message.contains("file_path") && message.contains("path"),
            "the refusal must name both arguments: {message}"
        );

        // The schema is where a model reads the name, so it has to be the new one -- with `path`
        // mentioned in the description, because a model that is told only about `file_path` never
        // learns why its old calls still work, and a person reading `debug prompt-input` sees the
        // same sentence.
        let schema = tools
            .specs()
            .into_iter()
            .find(|(name, _, _)| name == "read")
            .map(|(_, _, schema)| schema)
            .expect("read is one of the tools");
        assert!(
            schema["required"] == json!(["file_path"]),
            "`file_path` is what the tools ask for: {schema}"
        );
        assert!(
            schema["properties"]["file_path"]["description"]
                .as_str()
                .is_some_and(|text| text.contains("path")),
            "the description must own up to the alias: {schema}"
        );
    }

    /// And the gate is on the writing tools, not only on the helper.
    ///
    /// Measured before the fix: `write` to `NUL` answered "wrote 5 bytes" and the bytes went to
    /// the null device; `trailing.` answered the same and the file that appeared was
    /// `trailing`. A success message at a path that was not written is the failure this
    /// prevents, so the assertion is on the refusal and on the ordinary name still working.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_refuses_a_name_windows_would_rewrite() {
        let dir = TempDir::new("names");
        let config = Config {
            max_tool_output: 10_000,
            ..Config::default()
        };
        let tools = ToolBox::new(&config, false, dir.path().to_path_buf());
        // The reason is asserted, not merely that something failed: for `a:b.txt` the OS would
        // also fail, by resolving the name to a file on drive A, and a test that accepts any
        // error would pass without the check existing at all.
        for (name, reason) in [
            ("NUL", "reserved device name"),
            ("trailing.", "ends with a dot"),
            ("a:b.txt", "alternate data stream"),
        ] {
            let error = tools
                .invoke("write", &json!({ "path": name, "content": "hello" }))
                .await
                .expect_err("a rewritten name must be refused");
            let message = error.to_string();
            assert!(
                message.contains("cannot write") && message.contains(reason),
                "the refusal for `{name}` must say why ({reason}): {message}"
            );
        }
        let ok = tools
            .invoke("write", &json!({ "path": "fine.txt", "content": "hello" }))
            .await
            .expect("an ordinary name still writes");
        assert!(ok.contains("fine.txt"), "{ok}");
    }
    /// A `\n` `old_string` in a `\r\n` file must say why it did not match.
    ///
    /// `read` hands the bytes over as they are, so the model sees the `\r` -- but a model that
    /// then writes the replacement text the way it writes all text, with `\n`, gets "old_string
    /// not found" and no idea that a character it cannot see is the reason. It costs a turn, and
    /// the turn is spent guessing. Naming the line endings is the whole fix: flint must not
    /// normalise them, because silently rewriting every line of a file is a diff nobody asked
    /// for and the worst kind to review.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_edit_against_crlf_says_so_when_the_text_does_not_match() {
        let dir = TempDir::new("crlf");
        let config = Config {
            max_tool_output: 10_000,
            ..Config::default()
        };
        let tools = ToolBox::new(&config, false, dir.path().to_path_buf());
        std::fs::write(dir.path().join("dos.txt"), "one\r\ntwo\r\n").unwrap();
        tools
            .invoke("read", &json!({ "path": "dos.txt" }))
            .await
            .expect("read first");

        let error = tools
            .invoke(
                "edit",
                &json!({ "path": "dos.txt", "old_string": "two\n", "new_string": "three\n" }),
            )
            .await
            .expect_err("a \\n old_string cannot match a \\r\\n file");
        let message = error.to_string();
        assert!(
            message.contains("CRLF"),
            "the refusal must name the line endings: {message}"
        );

        // The same edit with the `\r` in it works, and the file keeps its endings.
        tools
            .invoke(
                "edit",
                &json!({ "path": "dos.txt", "old_string": "two\r\n", "new_string": "three\r\n" }),
            )
            .await
            .expect("an exact match still edits");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dos.txt")).unwrap(),
            "one\r\nthree\r\n"
        );

        // A `\n` file gets no line-ending lecture.
        std::fs::write(dir.path().join("unix.txt"), "one\ntwo\n").unwrap();
        tools
            .invoke("read", &json!({ "path": "unix.txt" }))
            .await
            .expect("read first");
        let error = tools
            .invoke(
                "edit",
                &json!({ "path": "unix.txt", "old_string": "nope", "new_string": "x" }),
            )
            .await
            .expect_err("no match is still an error");
        assert!(
            !error.to_string().contains("CRLF"),
            "an LF file must not be blamed for line endings: {error}"
        );
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
