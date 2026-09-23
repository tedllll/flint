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

/// What a command this run starts is told about the run it is in.
///
/// A command is not a run (`docs/agents.md`): it has no session file, no presence record and no
/// depth, so `flint who` does not name it and no listing of conversations holds it. What it *is* is
/// inside one, and until this existed the only way a script could tell was to notice the proxy
/// variables it had been handed. The reader this is for is the script the model writes: a build that
/// wants to leave a note for the run that started it, a test that wants the transcript's path, a
/// command that wants to ask the model a second question on the same endpoint.
///
/// The names follow Pi's convention (`PI_SESSION_FILE`, `PI_PROVIDER`, `PI_MODEL`) rather than
/// inventing a shape: a script's author who has met the idea elsewhere should not have to learn a
/// second vocabulary for it, and the prefix is what makes them unmistakably flint's.
///
/// **Set explicitly on every command, never left to inheritance.** This process may itself have been
/// started *by* a run's command -- a model running `flint -p ...` through `bash` is exactly that --
/// in which case the inherited values describe the run one level up. A fact that is sometimes the
/// parent's is worse than no fact, so `session: None` removes the name rather than leaving whatever
/// arrived.
#[derive(Clone, Default)]
pub struct RunEnv {
    /// The conversation this run is writing, absolute, or `None` for a run with none of its own.
    ///
    /// Enough to find everything else a run keeps: the transcript, the spill files and the logs of
    /// the jobs it started are all named from it (`<session>/`, `background-*.log`).
    pub session: Option<PathBuf>,
    /// The endpoint paying for this run, by the name `--provider` and `/provider` use.
    pub provider: String,
    /// The model in force, as the request body names it.
    pub model: String,
}

impl RunEnv {
    /// Write these facts onto a command that is about to be spawned.
    ///
    /// Private to this module: the two places that spawn a command for a model to reason about are
    /// `run_program_streaming` and `start_background_command`, and they both go through
    /// [`apply_child_env`] so that a fact added here reaches a foreground command and a background
    /// one alike. Two spawn sites that each grew their own copy is how the proxy variables came to
    /// be duplicated in the first place.
    fn apply(&self, cmd: &mut tokio::process::Command) {
        match &self.session {
            Some(path) => {
                cmd.env("FLINT_SESSION", path);
            }
            // Removed rather than skipped: an unset variable and an inherited stale one are not the
            // same answer to "which conversation am I in".
            None => {
                cmd.env_remove("FLINT_SESSION");
            }
        }
        // Empty means "this run has no endpoint of its own to name", which today is `flint exec` and
        // nothing else -- and it means *removed* for the same reason the session does. The first
        // version left an empty name unset, and a test that planted a stale `FLINT_PROVIDER` in
        // flint's own environment caught it arriving at the command: "not mentioned" and "taken
        // away" are not the same answer, and only one of them is true.
        if self.provider.is_empty() {
            cmd.env_remove("FLINT_PROVIDER");
        } else {
            cmd.env("FLINT_PROVIDER", &self.provider);
        }
        if self.model.is_empty() {
            cmd.env_remove("FLINT_MODEL");
        } else {
            cmd.env("FLINT_MODEL", &self.model);
        }
    }
}

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

    /// The configuration this tool hands to the children it starts, for the facts that can only be
    /// known later: which endpoint "same as here" means. `None` for every other tool, which is what
    /// makes the caller a two-line loop rather than a downcast.
    ///
    /// One method for both tools that start children, because they hold the same `TaskConfig`: a
    /// fact filled in here cannot reach `task` and miss `tasks`.
    fn task_config(&mut self) -> Option<&mut TaskConfig> {
        None
    }

    /// What a command this tool runs is told about the run it is in. `None` for every tool that
    /// runs no command, for [`Tool::task_config`]'s reason: the caller stays a two-line loop.
    ///
    /// Filled in after the tool is built, because the conversation's own path and the endpoint in
    /// force are known to `Agent::new` and not to `ToolBox::new` -- the same order
    /// [`ToolBox::with_task_endpoint`] explains.
    fn run_env(&mut self) -> Option<&mut RunEnv> {
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
    /// What this run tells the commands it starts, kept here as well as pushed into the tools so
    /// that a caller outside the tool set (the REPL's `!` escape) can hand it to a command it runs
    /// itself. One fact in one place: the escape and the `bash` tool must not describe different
    /// runs to the same script.
    run_env: RunEnv,
    /// Whether the tools are offered one at a time. See `Config::lazy_tools`.
    lazy: bool,
    /// The tools declared whatever `lazy` says -- `Config::eager_tools`, which is where
    /// `CONVENTIONAL_TOOLS` goes when a person pastes it.
    eager: Vec<String>,
    /// The tools the model has asked about, shared with the `tools` tool that adds to it.
    ///
    /// A `ToolBox` is built once and read on every request, so this is what makes the set grow
    /// during a run: the lookup tool writes, `specs` reads, and the next request declares one more.
    unlocked: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl ToolBox {
    pub fn new(config: &Config, readonly: bool, cwd: PathBuf) -> Self {
        // Skills are looked up in the same directories the prompt's catalog was built
        // from, so the tool can never offer something the catalog did not name.
        let skill_dirs = context::Workspace::discover(&cwd, &config.skill_dirs);
        // The session's own directory, until `with_spill_dir` names the real one. A tool
        // that writes a file of its own is built with it now rather than told later, so
        // that a `ToolBox` used without `with_spill_dir` still writes somewhere sane.
        let spill_dir = unattached_spill_dir();
        // One record for the whole tool set: it is the run's memory of what it has looked
        // at, so every tool that reads has to write into the same one that the writers
        // consult.
        let reads = std::sync::Arc::new(Reads::default());
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(BashTool {
                config: config.clone(),
                readonly,
                cwd: cwd.clone(),
                spill_dir: spill_dir.clone(),
                logs: std::sync::atomic::AtomicUsize::new(0),
                env: RunEnv::default(),
            }),
            Box::new(ExecTool {
                config: config.clone(),
                readonly,
                cwd: cwd.clone(),
                spill_dir: spill_dir.clone(),
                logs: std::sync::atomic::AtomicUsize::new(0),
                env: RunEnv::default(),
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
                config: TaskConfig {
                    cwd: cwd.clone(),
                    readonly,
                    provider: String::new(),
                    model: String::new(),
                    agents: skill_dirs.agents.clone(),
                    parent: None,
                    no_session: false,
                },
            }),
            // The fan-out, offered under the same conditions and with the same configuration: it is
            // the same child started more than once, so it costs no extra argument to keep in step.
            // Offered always, like `task`: two jobs of the same shape is an ordinary thing to want,
            // and a tool that appears and disappears is one a model cannot plan around.
            Box::new(TasksTool {
                config: TaskConfig {
                    cwd: cwd.clone(),
                    readonly,
                    provider: String::new(),
                    model: String::new(),
                    agents: skill_dirs.agents.clone(),
                    parent: None,
                    no_session: false,
                },
            }),
            // The handle for a child nobody waited for. It starts nothing and costs nothing to offer:
            // one schema with three words in it, against a `task` that has been able to leave a child
            // running since the fan-out existed.
            Box::new(JobOpTool),
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
        // The lookup itself, built last so its catalogue can name every tool the run has -- and
        // first in the list, because it is the one that is always sent.
        let unlocked: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        // Absent means all-lazy: the lookup and its catalogue, and every other tool obtained by
        // asking. A list is what the person asked for -- `CONVENTIONAL_TOOLS` is the one to paste
        // for a model that guesses rather than looking -- and is not second-guessed.
        let eager: Vec<String> = config.eager_tools.clone().unwrap_or_default();
        let catalogue = catalogue_of(&tools, config.lazy_tools, &eager);
        tools.insert(
            0,
            Box::new(ToolsTool {
                catalogue,
                unlocked: std::sync::Arc::clone(&unlocked),
            }),
        );
        let by_name = tools
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name().to_string(), i))
            .collect();
        ToolBox {
            lazy: config.lazy_tools,
            eager,
            unlocked,
            tools,
            by_name,
            max_output: config.max_tool_output,
            spill_dir,
            spilled: std::sync::atomic::AtomicUsize::new(0),
            run_env: RunEnv::default(),
        }
    }

    /// Tell the commands this run starts which run they are in.
    ///
    /// Kept here as well as pushed into the tools, because the REPL's `!` escape runs a command
    /// through `run_command_raw` rather than through a tool, and a person typing a command into a
    /// conversation must not be told a different run than the model's `bash` is.
    pub fn with_run_env(
        mut self,
        session: Option<PathBuf>,
        provider: &str,
        model: &str,
    ) -> Self {
        let env = RunEnv {
            session,
            provider: provider.to_string(),
            model: model.to_string(),
        };
        for tool in &mut self.tools {
            if let Some(slot) = tool.run_env() {
                *slot = env.clone();
            }
        }
        self.run_env = env;
        self
    }

    /// What this run tells a command it starts.
    pub fn run_env(&self) -> &RunEnv {
        &self.run_env
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
            if let Some(config) = tool.task_config() {
                config.provider = provider.to_string();
                config.model = model.to_string();
            }
        }
        self
    }

    /// Tell the children this run starts which conversation asked for them.
    ///
    /// Separate from the endpoint because it is a different fact: the endpoint is *how* a child talks,
    /// and this is *where it came from*. It reaches the child as `FLINT_PARENT`, which `main` reads to
    /// name the parent in the child's `meta` line and to file the child's session under `children/`,
    /// out of the person's list of their own conversations. `None` for a run that keeps no
    /// conversation (`--no-session`), which has nothing true to be the parent of -- and which is why
    /// that run also hands its children `--no-session`, below.
    pub fn with_task_parent(mut self, parent: Option<String>) -> Self {
        for tool in &mut self.tools {
            if let Some(config) = tool.task_config() {
                config.parent = parent.clone();
            }
        }
        self
    }

    /// Tell the `task` tool that this run keeps no conversation, so its children keep none either.
    ///
    /// A child is a conversation the *parent* asked for, so leaving it behind would mean `--no-session`
    /// writes nothing except through the one door it opened itself. The child is a whole flint started
    /// from `task_argv`, so the promise travels the same way the endpoint and the parent's name do.
    pub fn with_task_no_session(mut self, no_session: bool) -> Self {
        for tool in &mut self.tools {
            if let Some(config) = tool.task_config() {
                config.no_session = no_session;
            }
        }
        self
    }

    /// (name, description, JSON schema) for the tools this request declares.
    ///
    /// With `lazy_tools` on that is the lookup and whatever the model has asked about so far --
    /// see `ToolsTool`. The filter is here rather than in the caller so that there is one answer to
    /// "what can the model call right now", and the REPL, the page, `debug prompt-input` and a
    /// `task` child all get it by asking the same question.
    pub fn specs(&self) -> Vec<(String, String, Value)> {
        self.tools
            .iter()
            .filter(|t| self.is_declared(t.name()))
            .map(|t| {
                (
                    t.name().to_string(),
                    t.description().to_string(),
                    t.schema(),
                )
            })
            .collect()
    }

    /// Whether this request gives the model a tool, and so its arguments.
    ///
    /// One function because two places need the same answer: what `specs` sends, and whether a
    /// failed call should point at the lookup -- a call that got its arguments wrong is only
    /// interesting when the model was never given them.
    pub fn is_declared(&self, name: &str) -> bool {
        if !self.lazy {
            return true;
        }
        if name == "tools" || self.eager.iter().any(|e| e == name) {
            return true;
        }
        self.unlocked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name)
    }

    /// Every tool, whether or not this request declares it.
    ///
    /// For the things that are about the *set* rather than about one request: the `--readonly`
    /// refusal's wording, and the tests that hold the payload's size.
    pub fn all_specs(&self) -> Vec<(String, String, Value)> {
        self.tools
            .iter()
            .map(|t| (t.name().to_string(), t.description().to_string(), t.schema()))
            .collect()
    }

    pub async fn invoke(&self, name: &str, args: &Value) -> Result<String> {
        let Some(index) = self
            .by_name
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
        else {
            // A name that is not a tool at all. The listing is the only useful thing to say, and
            // this is the one place that knows it is missing rather than wrong.
            let held = if self.lazy { self.declared_names() } else { String::new() };
            return Err(anyhow!(
                "unknown tool '{name}'.{}{held}",
                if self.lazy { " Call `tools` to list what there is." } else { "" }
            ));
        };
        // The arguments came from somewhere, and if the model was never given them it guessed --
        // which is what it did when asked for `apply_patch`'s: it answered "name" and did not look
        // the tool up. The refusal is the moment it is willing to listen, so the refusal says where
        // the answer is. Only for a tool this request did not declare: for the rest the model has
        // the schema in front of it and a pointer would be noise.
        let was_declared = self.is_declared(name);
        let output = match self.tools[index].call(args).await {
            Ok(output) => output,
            // Only for an argument error: a call that failed at the *work* plainly had the
            // arguments it needed, and telling that model where the schema is would be noise on top
            // of a real failure.
            Err(e) if !was_declared && e.downcast_ref::<BadArguments>().is_some() => {
                return Err(anyhow!(
                    "{e:#}\n\n(`{name}` is not in your list yet, so its arguments were not given \
                     to you. Call `tools` with {{\"name\": \"{name}\"}} and it will be there from \
                     the next turn.)"
                ));
            }
            Err(e) => return Err(e),
        };
        Ok(self.cap(output))
    }

    /// The tools this request does not declare, named for a refusal that has to say what there is.
    fn declared_names(&self) -> String {
        let held: Vec<String> = self
            .tools
            .iter()
            .map(|t| t.name().to_string())
            .filter(|name| !self.is_declared(name))
            .collect();
        if held.is_empty() {
            String::new()
        } else {
            format!(" Held back for now: {}.", held.join(", "))
        }
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

/// The spill directory of a run that has no conversation of its own: one per process.
///
/// `spill/unattached/` was one directory for every session-less run on the machine, and the files in
/// it are numbered from 1 (`Tool::spill`), so two runs told to write no conversation wrote over each
/// other's `1.txt` -- and the second run's bytes are what the first one's request was told to read.
/// The pid is the smallest true name for "this process", and a name that says only which process it
/// was is honest about being disposable: nothing reads these directories back by name.
///
/// Used in two places that must agree: `ToolBox::new`'s fallback (a tool set that was never told
/// where to spill) and `Agent::new`, which decides the same thing from the other end -- whether there
/// is a conversation to name the directory after.
pub fn unattached_spill_dir() -> PathBuf {
    crate::config::spill_dir().join(format!("unattached-{}", std::process::id()))
}

/// A tool's path argument, as the path it means.
///
/// The rule -- `~` is the home directory, absolute is absolute, the rest is against the working
/// directory -- is [`crate::config::resolve_path`], shared with the page's routes and a person's
/// `@name`, because a model that writes `~/notes.txt` means the same file in all three places. This
/// name stays because eight call sites here read better for it, and because *why a tool resolves a
/// path at all* belongs beside the tool.
fn resolve_path(cwd: &Path, raw: &str) -> PathBuf {
    crate::config::resolve_path(cwd, raw)
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

/// An error from reading a tool's arguments, as opposed to one from doing the work.
///
/// `ToolBox::invoke` uses the distinction to help a model that was never given a tool's schema
/// without also explaining itself to one that was: an `edit` whose `old_string` did not match is a
/// call that plainly *had* its arguments, and Windows CI said so in as many words -- the pointer
/// meant for a guess was appended to the CRLF hint, turning a one-line hint into two paragraphs,
/// and the test that holds "the hint is one line" failed.
#[derive(Debug)]
pub(crate) struct BadArguments(pub String);

impl std::fmt::Display for BadArguments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadArguments {}

/// The error a tool returns when the arguments it was given are missing or the wrong shape.
fn bad_arguments(message: String) -> anyhow::Error {
    BadArguments(message).into()
}

pub(crate) fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    match args.get(key) {
        Some(Value::String(text)) => Ok(text),
        Some(other) => Err(bad_arguments(format!(
            "argument '{key}' must be a string, but it is {}",
            type_name(other)
        ))),
        None => Err(bad_arguments(format!("missing required string argument '{key}'"))),
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
        (Some(named), Some(older)) if named != older => Err(bad_arguments(format!(
            "arguments 'file_path' and 'path' name different files ({named} and {older}); \
             they are the same argument and only one may be given"
        ))),
        (Some(named), _) => Ok(named),
        (None, Some(older)) => Ok(older),
        (None, None) => Err(bad_arguments(
            "missing required string argument 'file_path'".to_string(),
        )),
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
        Some(other) => Err(bad_arguments(format!(
            "argument '{key}' must be a string, but it is {}",
            type_name(other)
        ))),
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
    /// Where a background command's log goes, and how many this session has started.
    ///
    /// The same directory the spilled output and the `pwsh` scripts go to, for the same reason: a
    /// session's files belong together, where a person reading the transcript can find them.
    spill_dir: PathBuf,
    logs: std::sync::atomic::AtomicUsize,
    /// What this command is told about the run that asked for it, filled in by
    /// [`ToolBox::with_run_env`].
    env: RunEnv,
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
///
/// The one route for anything below the REPL that has to say something and must not write to
/// stderr: with the answer strip active, a stray write lands wherever the cursor is. The sink is
/// installed by the CLI and wraps `Term::notice`, which commits the half-drawn answer first; the
/// fallback below is for a run with no UI at all (a test, an embedder, `--json`), where stderr is
/// the parseable place for it.
pub fn notice(message: &str) {
    match NOTICE.get() {
        Some(sink) => sink(message),
        // No UI has claimed the notices (a test, or an embedder): stderr is still better
        // than silence, and there is no layout to protect.
        None => eprintln!("flint: {message}"),
    }
}

/// Which of the two things a job can be: a run this one started, or a command it started.
///
/// One record for both, because a handle is a handle: the same pid, the same budget enforced
/// whether or not anybody is waiting, the same notice when it ends. The differences are three, and
/// each is about what the job *produces* -- a child answers in a session file and is asked to stop
/// by writing `/stop` to its stdin, while a command writes to a log file and can only be killed.
/// Keeping the kind on the job is what lets one verb answer for either without a model having to
/// remember which tool it started a thing with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Child,
    Command,
}

/// A job this run started, from the moment it is spawned until the last call that asks about it.
///
/// It used to be a note that a child was running -- for the one sentence a dropped turn has to be
/// honest with. It is now the whole handle, because that sentence and a handle are the same fact: a
/// run that starts a child and does not wait for it needs somewhere the answer will be, and the
/// presence record says a process is alive without saying what it answered. Everything a later tool
/// call needs is here: where its conversation is, whether it has ended, what it said, and the stdin
/// that is the only way to ask it to stop.
///
/// A background *command* is the same record with a different `kind`, and that is the whole of the
/// extension: what it produced is a log file instead of a conversation, and `stop` is a kill
/// because there is no conversation to interrupt. The pid, the budget, the `reported` bookkeeping
/// and the notice are shared, which is why there is one `job_op` rather than one verb per kind of
/// thing a run can leave running.
/// A moment in a job's life, in the two clocks that are each good for one thing: `at` is how long
/// ago it was, and `wall` is the time a person and a page read.
///
/// The pair is recorded together and never recomputed, which is the whole point of it existing.
/// `now - elapsed` looks equivalent and is not: both ends are truncated to whole seconds, so which
/// second comes back depends on where the fractional part of the clock happens to fall, and two looks
/// at the same running job can land a second apart. Windows CI found that once; the test beside the
/// snapshot test finds it on purpose.
///
/// The comparison is `at` first, which is the monotonic clock and therefore the right one for
/// ordering -- two jobs are sorted by when they started, not by whether the wall clock was adjusted
/// between them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct JobMoment {
    at: std::time::Instant,
    wall: std::time::SystemTime,
}

impl JobMoment {
    fn now() -> Self {
        Self {
            at: std::time::Instant::now(),
            wall: std::time::SystemTime::now(),
        }
    }

    fn elapsed(&self) -> std::time::Duration {
        self.at.elapsed()
    }
}

struct Job {
    kind: JobKind,
    pid: u32,
    label: String,
    /// What it was asked, cut short: a status listing that does not say which job this is makes a
    /// person read the session file to tell two children apart. For a command this is its command
    /// line, which is the same question ("which one is this?") with the same answer.
    prompt: String,
    started: JobMoment,
    /// Set when the child ended, so "finished 40s ago" is answerable without waiting for anything.
    ended: std::sync::Mutex<Option<JobMoment>>,
    depth: u32,
    readonly: bool,
    /// Whether this run started it *without waiting* -- which is what makes ending worth a notice of
    /// its own: a job somebody is waiting for hands its answer over in the tool result, and a line
    /// about it as well would be the same news twice.
    background: bool,
    /// Whether this job was asked to keep no conversation, so that "its session is not named yet" is
    /// never said about a child that will never name one. Always `false` for a command: a command has
    /// no conversation at all, and its line already says what it produced instead.
    no_session: bool,
    /// Whether what it produced has been read or handed over already.
    ///
    /// The one piece of bookkeeping DSH's job runtime has and this did not: an exit is news once. A
    /// `wait`, a `stop` and the report itself all set it, so a model that collected the answer is not
    /// told the job ended afterwards, and a model that was told is not told again on the next request.
    reported: std::sync::atomic::AtomicBool,
    /// Whether *this run* is what ended it.
    ///
    /// A kill's exit status is the shell's, not the command's: on Windows a `taskkill /T /F` leaves
    /// `cmd.exe` reporting 1, so a job a person stopped deliberately would be listed as `failed` --
    /// the one word that sends somebody looking for a bug that is not there. Measured while building
    /// `/jobs stop`: the same stop on Unix reports -1 (a signal) and read correctly, which is exactly
    /// the kind of difference a status word has to be independent of. So the truth is recorded where
    /// the decision is made rather than inferred from a number afterwards.
    ended_by_us: std::sync::atomic::AtomicBool,
    /// Whether this run has *asked* it to stop and it has not gone yet.
    ///
    /// A third piece of the same story as `ended_by_us`, one moment earlier: a stop is not an
    /// instant -- a child finishes the thought it was on and a killed process tree takes as long as
    /// the kill takes -- and the listing somebody reads during that moment is the one that would
    /// otherwise lie in the most expensive direction. `running` is true and useless there ("is it
    /// still doing what I asked, or is it on its way out?"), and `ended` would be a promise the run
    /// cannot make yet. So the word the flag buys is `stopping`, and it is deliberately read only
    /// against `finished`: a job that ended under a stop is described as ended, never as stopping
    /// after the fact.
    stopping: std::sync::atomic::AtomicBool,
    /// The budget that is enforced on it whether or not anyone is waiting.
    timeout_secs: u64,
    /// The child's stdin, kept here rather than in the future that started it: `/stop` has to be
    /// writable from a *later* turn, and the future that spawned the child is long gone by then.
    stdin: std::sync::Mutex<Option<tokio::process::ChildStdin>>,
    /// Where its session is, once it has said. Learned by the reader from `session.started`.
    session: std::sync::Mutex<Option<String>>,
    /// Told when it says, so a handle can name the conversation without polling for it.
    session_known: tokio::sync::Notify,
    /// What it answered, once it has ended. `None` means still running.
    finished: std::sync::Mutex<Option<Finished>>,
    /// Told when it ends, so a wait is a wake-up rather than a loop of sleeps.
    done: tokio::sync::Notify,
    /// The schema handed to it through a file, deleted once it has ended.
    schema_file: std::sync::Mutex<Option<PathBuf>>,
    /// Where a background *command* is writing: the file both streams go into, and the answer a
    /// `wait` hands over. `None` for a child, whose output is its conversation.
    log: Option<PathBuf>,
    /// How much of that file has already been handed over by `output`, so that reading a running
    /// command twice is two windows on it rather than the same log twice.
    read_offset: std::sync::Mutex<u64>,
}

/// How a child ended, and everything it said on the way.
#[derive(Clone)]
struct Finished {
    code: i32,
    /// The extra sentence the wait itself has to contribute: stopped after Ns, or killed.
    note: String,
    stderr: String,
    collected: Collected,
}

/// The children this process has started.
///
/// A global for the same reason the two sinks above are, and one more: the code that has to be honest
/// about a child -- `Agent::close_dangling_tool_calls`, which repairs the conversation after the wait
/// for it has already been dropped -- cannot ask the tool anything, because the tool's future is the
/// thing that was dropped. Measured on a real session: a `task` child outlived the turn that started
/// it by minutes, and the parent's record said the tool "was requested but never ran". Both halves of
/// that are wrong, and this is the only place the truth survives the drop.
///
/// It is process state and not persisted state, which is the honest scope of a handle: the children
/// *this* run started and has not forgotten. A run in another process is found through its presence
/// record, which is why that record now carries the conversation as well as the pid.
static CHILDREN: std::sync::OnceLock<std::sync::Mutex<Vec<std::sync::Arc<Job>>>> =
    std::sync::OnceLock::new();

/// How many finished children are kept around to be asked about.///
/// A long REPL session starts children forever, and each one held here holds its answer in memory.
/// Eight is more than anyone collects by pid in one sitting, and a child that fell off the end is
/// still on disk: its session file is the record, which is the rule everywhere else in this project.
const KEEP_FINISHED: usize = 8;

fn jobs() -> std::sync::MutexGuard<'static, Vec<std::sync::Arc<Job>>> {
    CHILDREN
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Every child this run started, running first, newest first.
fn jobs_listed() -> Vec<std::sync::Arc<Job>> {
    let mut jobs = jobs().clone();
    jobs.sort_by_key(|job| (job.has_finished(), std::cmp::Reverse(job.started)));
    jobs
}

/// A counter that changes whenever a job is registered or settles.
///
/// The page cannot see this process's memory, and a job that ends while nobody is waiting for it
/// happens between requests -- so the one thing the web feed needs is "the list is not what it was",
/// which is a number rather than a copy of the list: the route (`GET /jobs`) is where the list
/// lives, and a frame that carried the jobs would be a second answer that can disagree with it.
/// The same shape as the `sessions` frame, and for the same reason.
///
/// A number rather than a flag, because a reader that missed one change must still see the next:
/// only the *difference* matters, and a counter cannot lose one the way a boolean cleared by two
/// readers can.
pub fn jobs_revision() -> u64 {
    JOBS_REVISION.load(std::sync::atomic::Ordering::SeqCst)
}

static JOBS_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn jobs_changed() {
    JOBS_REVISION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// The jobs this run started, as the page's list shows them.
///
/// `GET /jobs` answers with this, and the shape is the page's rather than the model's: `job_op
/// status` writes a sentence for a reader who is thinking, and this is a row for a reader who is
/// scanning. Three of its fields exist because the page has no clock of the server's:
///
///   * `started_secs` and `ended_secs` are absolute epoch seconds, so a page that has been open for
///     an hour shows the true age of a job rather than counting from whenever it last heard. Both are
///     read off the moment the job recorded for itself -- see `JobMoment` -- rather than computed from
///     the clock at the time of the look, because a start that can move by a second between two
///     glances is not a start.
///   * `status` is a word rather than a sentence -- `running`, `stopping`, `completed`, `killed`,
///     `failed` -- and `detail` carries the fact sentence beside it, because a row needs a word it can
///     colour and a person wants the exit code. `stopping` is the one that is not read off the exit
///     code: a job that has been asked to stop and has not gone yet has no exit code to be read.
///   * `path` is where the job's output is: a child's conversation, or a command's log. It is what a
///     row opens, through the same `GET /file` a path in the transcript uses.
///
/// Nothing here is a new record: every field is read off the `Job` the handle already is, which is
/// why the page cannot come to disagree with `job_op`.
pub fn jobs_snapshot() -> serde_json::Value {
    let jobs: Vec<serde_json::Value> = jobs_listed()
        .iter()
        .map(|job| {
            let finished = job.finished.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let ended = *job.ended.lock().unwrap_or_else(|e| e.into_inner());
            let (status, detail) = match &finished {
                Some(finished) => (
                    job_status(finished.code, job.was_ended_here()),
                    format!("exit code {} ({})", finished.code, task_exit_meaning(finished.code)),
                ),
                // Not ended yet, and the flag is what says whether it is still working or already on
                // its way out -- read against the same `finished` the arm above is, so the word cannot
                // outlive the state. The panel draws this row while somebody is watching a stop land,
                // which is exactly when `running` would be the wrong word.
                None if job.is_stopping() => ("stopping", "asked to stop, not gone yet".to_string()),
                None => ("running", String::new()),
            };
            let path = match job.kind {
                JobKind::Child => job.session(),
                JobKind::Command => job.log.as_ref().map(|path| path.display().to_string()),
            };
            serde_json::json!({
                "pid": job.pid,
                "kind": match job.kind {
                    JobKind::Child => "child",
                    JobKind::Command => "command",
                },
                // What it was asked, which is the row's own text: a command's command line, or a
                // child's prompt. There is deliberately no separate "which tool started it" field --
                // for a child that word is `task`/`tasks`, and for a command `job.label` *is* the
                // command line, so a field carrying it would be the same string twice in one row.
                "label": util::truncate(&job.prompt, 200),
                "status": status,
                "detail": detail,
                "started_secs": epoch_secs(job.started.wall),
                "ended_secs": ended.map(|ended| epoch_secs(ended.wall)),
                "path": path,
            })
        })
        .collect();
    serde_json::json!({ "jobs": jobs })
}

/// The one word for how a job ended, from the exit code and from whether *this* run ended it.
///
/// -1 is what an exit status carries when a signal ended the process, so it is a different fact from
/// a failure and gets a different word: "it said it was done", "it broke" and "it was killed" are
/// the three answers a person is looking for in a list of work, and a row that said "failed" for a
/// kill would send somebody looking for a bug that is not there.
///
/// `ended_by_us` is the second half, and it is not decoration. A child asked to stop ends with **130**
/// ("the run was stopped"), which read from the code alone is `failed` -- the one word §8 of
/// `docs/features.md` promises a kill never gets. The flag is set before the signal goes out, so it
/// is true of every job this run ended; a job that finished with 0 in that same instant still reads
/// `completed`, because it did.
fn job_status(code: i32, ended_by_us: bool) -> &'static str {
    match code {
        0 => "completed",
        -1 => "killed",
        _ if ended_by_us => "killed",
        _ => "failed",
    }
}

/// When something that happened `ago` before `now` happened, in epoch seconds.
///
/// The job holds an `Instant` because that is what measures an elapsed time correctly, and the page
/// needs a wall clock because that is what survives a page that was open across the two. This is the
/// one conversion between them, and it is derived rather than stored: a second field on the job
/// would be the same fact recorded twice, and the rule here is that only one of them can be wrong.
fn epoch_secs(wall: std::time::SystemTime) -> u64 {
    wall.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One child by pid, when this run is the one that started it.
fn job_by_pid(pid: u32) -> Option<std::sync::Arc<Job>> {
    jobs().iter().find(|job| job.pid == pid).cloned()
}

/// Forget the oldest finished children, so that a long session does not grow one answer per child.
fn forget_old_finished() {
    let mut jobs = jobs();
    let mut finished: Vec<usize> = jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| job.has_finished())
        .map(|(index, _)| index)
        .collect();
    if finished.len() <= KEEP_FINISHED {
        return;
    }
    finished.sort_by_key(|index| jobs[*index].started);
    let drop_count = finished.len() - KEEP_FINISHED;
    for index in finished.into_iter().take(drop_count).rev() {
        jobs.remove(index);
    }
}

impl Job {
    fn has_finished(&self) -> bool {
        self.finished
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn session(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The one line a status listing gives for this child.
    fn describe(&self) -> String {
        // Where to go for what it produced: a child's answer is a conversation, a command's is a
        // file. Everything else about the line is the same question asked of either.
        let produced = match self.kind {
            JobKind::Child => match (self.session(), self.no_session) {
                (Some(path), _) => format!("session {path}"),
                // A child that will never name one is not "not yet anything": it was started with
                // `--no-session`, so the file is not late, it is not coming.
                (None, true) => "it keeps no conversation (--no-session)".to_string(),
                (None, false) => "its session is not named yet".to_string(),
            },
            JobKind::Command => match &self.log {
                Some(path) => format!("output in {}", path.display()),
                None => "it has no output file".to_string(),
            },
        };
        let collecting = match self.kind {
            JobKind::Child => "`wait` collects what it said",
            JobKind::Command => "`wait` collects its output, `output` reads what is new",
        };
        // "running" and "stopping" are the only two words for a job that has not ended, and the
        // difference is the one a person asks about while they wait: what it produces from here is
        // what it already had (a child's half of an answer, a command's log), not what comes next.
        // The clock is the same one either way -- the ask is not a second start.
        let state = if self.is_stopping() {
            "stopping"
        } else {
            "running"
        };
        match self.finished.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(finished) => format!(
                "pid {}: {} -- ended with exit code {} ({}), {}; {}",
                self.pid,
                self.label,
                finished.code,
                task_exit_meaning(finished.code),
                produced,
                collecting
            ),
            None if self.kind == JobKind::Command => format!(
                "pid {}: {} -- {} for {}, {}",
                self.pid,
                self.label,
                state,
                elapsed_label(self.started.elapsed()),
                produced
            ),
            None => format!(
                "pid {}: {} -- {} for {}, {}; asked: {}",
                self.pid,
                self.label,
                state,
                elapsed_label(self.started.elapsed()),
                produced,
                util::truncate(&self.prompt, 80)
            ),
        }
    }

    /// Register the fact that somebody has read what this job produced.
    fn mark_reported(&self) {
        self.reported
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn was_reported(&self) -> bool {
        self.reported.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Note that this run is ending it, before the signal goes out.
    ///
    /// Before, not after: the supervisor records the exit status as soon as it has one, and a flag
    /// set afterwards would sometimes lose the race it exists to win.
    fn ended_here(&self) {
        self.ended_by_us
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn was_ended_here(&self) -> bool {
        self.ended_by_us.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Note that the ask has gone out, before anything waits for it to land.
    ///
    /// Before, for `ended_here`'s reason: whoever looks next sees the state that is true *then*, and
    /// the wait that follows is exactly the moment somebody looks.
    fn asked_to_stop(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether it has been asked and has not gone yet -- the whole of the `stopping` word, in one
    /// place, because reading the flag without `finished` beside it would say "stopping" about a job
    /// that ended a minute ago.
    fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::SeqCst) && !self.has_finished()
    }

    /// Wait for the child to end, or give up after `limit`.
    ///
    /// The notify is registered before the value is read, so a child that ends between the two is
    /// still seen: the wake-up is already armed when the check happens, which is what makes this a
    /// wait rather than a race.
    ///
    /// Getting an answer out of this *is* reading it, so it counts as reported: everything that
    /// reports a finished job goes through here, and a second telling would be noise.
    async fn wait(&self, limit: Option<std::time::Duration>) -> Option<Finished> {
        let deadline = limit.map(|limit| tokio::time::Instant::now() + limit);
        loop {
            let notified = self.done.notified();
            if let Some(finished) = self
                .finished
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                self.mark_reported();
                return Some(finished);
            }
            match deadline {
                None => notified.await,
                Some(deadline) => {
                    if tokio::time::timeout_at(deadline, notified).await.is_err() {
                        return None;
                    }
                }
            }
        }
    }

    /// Wait for the child to name its conversation, which it does before it asks the model anything.
    async fn wait_session(&self, limit: std::time::Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let notified = self.session_known.notified();
            if let Some(path) = self.session() {
                return Some(path);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return None;
            }
        }
    }

    /// Ask the child to stop, the way a person stops a run.
    ///
    /// A request, not a kill: `/stop` is the word the terminal takes, and the child commits the half
    /// of an answer it had drawn before it goes. A child that ignores it is ended by its own timeout,
    /// which is enforced by the supervisor rather than by whoever happens to be waiting.
    async fn ask_to_stop(&self) -> bool {
        use tokio::io::AsyncWriteExt;
        // Lifted out of the slot rather than borrowed across the write: a `std::sync::MutexGuard`
        // held across an await would make the whole supervisor future `not Send`, and this is the one
        // await in it. It goes back afterwards so that a second ask, or the supervisor's own timeout,
        // can still reach the same child.
        let mut stdin = match self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(stdin) => stdin,
            None => return false,
        };
        let mut wrote = stdin.write_all(b"/stop\n").await.is_ok();
        wrote = wrote && stdin.flush().await.is_ok();
        *self.stdin.lock().unwrap_or_else(|e| e.into_inner()) = Some(stdin);
        // Only when the word actually went out: a failed write means the child's stdin is closed,
        // which is the one thing that says it has already ended -- and calling that `stopping` would
        // contradict the `ended` line the very next look prints.
        if wrote {
            self.asked_to_stop();
        }
        wrote
    }

    /// End a background *command*.
    ///
    /// There is nothing to ask: a command has no conversation to interrupt and no half of an answer
    /// to commit, so this is a kill -- and it has to be the whole tree. `kill_on_drop` ends only the
    /// process that was spawned, and on Windows the shell stays as the parent of whatever it ran:
    /// the measurement in `KillTree`'s comment is about exactly this (a killed `cmd.exe` left
    /// `cargo`/`rustc` holding `target/`). On Unix the shell normally *becomes* the command, but a
    /// script that backgrounds work does not, which is why that side gets a process group
    /// ([`KillTree::detach`]) and this signals the group rather than one process.
    ///
    /// Blocking, and on purpose: the caller is a tool call that has already decided to end this, and
    /// `taskkill` measures in the hundreds of milliseconds. On Unix there is nothing to wait for at
    /// all -- it is one `killpg`, which is also the reason it is a syscall here rather than the
    /// `kill` command: no shell spells "the group" the same way twice, and the tool that started
    /// this is not going to hand a group id to a parser it does not control.
    fn kill(&self) {
        // Before the signal, so the status word this ends up under is the one that is true: see
        // `ended_by_us`. The `stopping` flag goes up here too, one moment before `ended_by_us` is
        // read: a kill is not the same as an end, and the time between them is exactly the window a
        // listing is likely to be read in.
        self.asked_to_stop();
        self.ended_here();
        #[cfg(windows)]
        {
            let pid = self.pid.to_string();
            let mut command = std::process::Command::new("taskkill");
            command.args(["/PID", &pid, "/T", "/F"]);
            let _ = command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(unix)]
        unsafe {
            libc::killpg(self.pid as libc::pid_t, libc::SIGKILL);
        }
    }

    /// What a background command has printed since the last time somebody asked.
    ///
    /// Incremental on purpose: a build prints for ten minutes, and a verb that handed over the whole
    /// log on every call would put the same thousands of lines into the request again and again. The
    /// cursor lives on the job, so "since the last time" survives the call that asked -- and reading
    /// to the end of a job that has *ended* is reading all of it, which counts as collecting it.
    fn new_output(&self) -> String {
        let Some(path) = &self.log else {
            return "this job has no output file: it is a child, and what a child produced comes from \
                    `wait`."
                .to_string();
        };
        let bytes = std::fs::read(path).unwrap_or_default();
        let start = {
            let offset = self.read_offset.lock().unwrap_or_else(|e| e.into_inner());
            // Saturating rather than trusting: the file is only ever appended to, but a person can
            // truncate it by hand, and a stale offset must not panic or skip the new output.
            (*offset as usize).min(bytes.len())
        };
        let fresh = String::from_utf8_lossy(&bytes[start..]).to_string();
        *self.read_offset.lock().unwrap_or_else(|e| e.into_inner()) = bytes.len() as u64;
        if fresh.trim().is_empty() {
            return format!(
                "nothing new from pid {} ({}); it is {}",
                self.pid,
                path.display(),
                if self.has_finished() { "over" } else { "still running" }
            );
        }
        if self.has_finished() {
            // Everything it will ever print has now been handed over, so the notice that it ended
            // would be news the caller already has.
            self.mark_reported();
        }
        fresh
    }

    /// The child's answer, in the shape a caller reads.
    fn render(&self, finished: &Finished) -> String {
        if self.kind == JobKind::Command {
            return self.render_command(finished);
        }
        let collected = &finished.collected;
        let mut text = match collected.answer() {
            Some(answer) => answer,
            // Nothing on the stream at all: the reason is on stderr, and passing it on is the
            // difference between "the child failed" and "the child refused before it started".
            None => finished.stderr.trim().to_string(),
        };
        if text.trim().is_empty() {
            text = "(the child said nothing)".to_string();
        }
        let code = finished.code;
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
        text.push_str(&finished.note);
        text.push_str(&format!(
            "\ndepth: {} (this run is {}), readonly: {}, waited: {:.0}s",
            self.depth,
            self.depth - 1,
            self.readonly,
            self.started.elapsed().as_secs_f64()
        ));
        match collected.session.clone().or_else(|| self.session()) {
            Some(session) => text.push_str(&format!("\nsession: {session}")),
            // Said rather than left out. A caller's habit is to follow up inside the child's
            // conversation, so a result with no `session:` line reads as one that has not been named
            // *yet* -- and this one never will be, because it was started with `--no-session`.
            None if self.no_session => text.push_str("\nsession: none (--no-session)"),
            None => {}
        }
        text
    }

    /// A background command's answer: the file it wrote to, and how it ended.
    ///
    /// The whole file, even the part an `output` call already showed: this is the answer, and a
    /// caller that asked for it by name is not being told something twice. It is the log rather than
    /// a summary because for a command the output *is* the answer -- a build's last twenty lines are
    /// the whole result of a twenty-minute build.
    fn render_command(&self, finished: &Finished) -> String {
        let mut text = match &self.log {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) if !text.trim().is_empty() => text,
                Ok(_) => "(it printed nothing)".to_string(),
                Err(e) => format!("(its output could not be read: {e})"),
            },
            None => "(it has no output file)".to_string(),
        };
        text.push_str(&format!(
            "\n\nexit code: {} ({})",
            finished.code,
            task_exit_meaning(finished.code)
        ));
        if let Some(path) = &self.log {
            text.push_str(&format!("\noutput: {}", path.display()));
        }
        text.push_str(&finished.note);
        // No depth and no readonly here, unlike a child: a command is not a run, so there is no
        // chain for it to be part of and no second context for the flag to have been decided in.
        text.push_str(&format!("\nran for {:.0}s", self.started.elapsed().as_secs_f64()));
        text
    }
}

/// The children this run has left running, in the words a caller needs. Empty when there are none.
///
/// One line each. What the line is *for* is the warning not to ask for the same work again -- the
/// tokens are already spent, and a model that cannot see where the answer is going spends them twice.
/// The session path is how that promise is usually kept: a child is a run of its own, so its answer is
/// being written to a file of its own *whatever happens to this one*, which is what turns "the task was
/// interrupted" from a loss into a place to go and look. A child that has not named its conversation
/// yet has no path to give, and that is a measured state rather than a theoretical one (see
/// `answer_location`): the line stays honest about it and hands over the *verb* instead, because the
/// promise being kept is "the work is not lost and is not worth paying for again", not "here is a
/// path".
pub fn children_running() -> Vec<String> {
    jobs()
        .iter()
        .filter(|job| !job.has_finished())
        .map(|child| {
            let mut line = match child.kind {
                JobKind::Child => format!(
                    "a child this run started is still going on its own: {} (pid {}, running for {})",
                    child.label,
                    child.pid,
                    elapsed_label(child.started.elapsed())
                ),
                JobKind::Command => format!(
                    "a command this run started in the background is still running: {} (pid {}, \
                     running for {})",
                    child.label,
                    child.pid,
                    elapsed_label(child.started.elapsed())
                ),
            };
            line.push(' ');
            line.push_str(&answer_location(
                &child.kind,
                child.session().as_deref(),
                child.log.as_deref(),
            ));
            line
        })
        .collect()
}

/// Where a running job's answer is, in words a reader can act on -- a *complete sentence*, punctuated
/// here and in no caller.
///
/// That last part is the fix for a measured defect rather than a style: every caller used to append
/// its own full stop to a sentence that already ended in one, so a dropped turn's note read
/// "…rather than asking for the same work again.." and "It has not named its session yet..". Two
/// writers punctuating one sentence is a mistake with no way to be right; one is a sentence that
/// cannot be wrong twice.
///
/// Four shapes, and each is an answer rather than a fallback for another. A child that has named its
/// conversation has a file to read. A child that has *not* has a verb to ask instead -- the pid in
/// the line's head is the handle `job_op` takes -- and saying so matters more than it looks: this
/// sentence exists to stop the same work being paid for twice, so a dead end here is read as "nothing
/// to collect, run it again". A command has a log file, which is the whole difference between the two
/// kinds; and a command without one still says so rather than leaving the sentence hanging.
fn answer_location(kind: &JobKind, session: Option<&str>, log: Option<&Path>) -> String {
    match (kind, session, log) {
        (JobKind::Child, Some(path), _) => format!(
            "Its answer is being written to its own session, {path} -- read it there rather than \
             asking for the same work again."
        ),
        (JobKind::Child, None, _) => "It has not named its conversation yet -- ask `job_op` for it \
             with `action: \"status\"` rather than asking for the same work again."
            .to_string(),
        (JobKind::Command, _, Some(path)) => format!(
            "Its output is going to {} -- read it there, or ask `job_op` for it, rather than running \
             the same command again.",
            path.display()
        ),
        (JobKind::Command, _, None) => "It has no output file.".to_string(),
    }
}

/// What a job that has *ended* contributes, to the model and to the person.
impl Job {
    /// The line a settled job contributes to the report the model is given on its next request.
    ///
    /// Written for a model that has moved on: what ended, whether it worked, how to get the answer,
    /// and where the answer is in case the verb is not what it wants. It names the pid because the pid
    /// is the handle it was given -- a report about "a job" would leave it looking for one.
    fn settle_line(&self) -> String {
        let Some(finished) = self
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return String::new();
        };
        let mut line = format!(
            "pid {} ({}) finished: exit code {} ({}), after {}. Nothing has collected what it {}; \
             `job_op` action \"wait\" with pid {} gets it",
            self.pid,
            self.label,
            finished.code,
            task_exit_meaning(finished.code),
            elapsed_label(self.started.elapsed()),
            // `wait` for either kind, deliberately: it is the verb that hands over the whole of what
            // a job produced, and a report that taught a second verb for the same move would be
            // teaching the model a distinction that only matters while the job is still running.
            match self.kind {
                JobKind::Child => "said",
                JobKind::Command => "printed",
            },
            self.pid
        );
        match self.session() {
            Some(path) => line.push_str(&format!(". Its conversation: {path}.")),
            None => match &self.log {
                Some(path) => line.push_str(&format!(". Its output: {}.", path.display())),
                None => line.push('.'),
            },
        }
        line
    }

    /// The same news for the person, who has no tools to call.
    ///
    /// Deliberately says no verb and no pid-as-a-handle: it is a line in their transcript, and what a
    /// person does about a job that ended is say something. The pid is still there because it is how
    /// the same job is named in every other thing they can see (`flint who`, a process listing).
    fn finished_notice(&self) -> String {
        let code = self
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|finished| finished.code)
            .unwrap_or(-1);
        let mut line = format!(
            "{} (pid {}) finished -- exit code {} ({})",
            self.label,
            self.pid,
            code,
            task_exit_meaning(code)
        );
        match self.session() {
            Some(path) => line.push_str(&format!("; its answer is in {path}")),
            None => match &self.log {
                Some(path) => line.push_str(&format!("; its output is in {}", path.display())),
                None => line.push_str("; it never named a conversation"),
            },
        }
        line
    }
}

/// The jobs this run started that have ended and that nobody has read, one line each.
///
/// This is the notice DSH's job runtime calls a completion notice, in the shape a request can carry
/// it: a settled job the model was never told about. `mark` is the difference between asking what
/// would be said and saying it -- the request that carries the report marks it, so the next request is
/// not told the same thing again, while `flint debug prompt-input` asks without marking, which is what
/// a preview has to be.
pub fn settled_unreported(mark: bool) -> Vec<String> {
    let mut lines = Vec::new();
    for job in jobs().iter() {
        if job.has_finished() && !job.was_reported() {
            lines.push(job.settle_line());
            if mark {
                job.mark_reported();
            }
        }
    }
    lines
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

/// What `timeout_secs` means, for the two tools that take one.
///
/// Written once because it was written twice -- in `bash` and in `exec` -- and the two copies had
/// already drifted into different words for the same rule. Every request pays for every word of
/// every schema, on every turn, for the life of the tool; a paragraph that exists in two tools is
/// paid for twice, for ever, to say one thing.
const TIMEOUT_SECS: &str = "Kill it after this many seconds (default 120; 900 when it installs, \
                            builds, downloads or runs in the background). Raise it for anything \
                            known to be slow.";

/// What `background: true` does, for the two tools that wait by default.
///
/// The same paragraph was in `bash` and in `exec`, differently worded in each. `task` waits by
/// default too, with the opposite default, so it keeps its own -- see `TaskTool`.
const BACKGROUND_WAITS: &str = "Default false: wait here and return the output. Set true for \
                                anything slow, and the call returns at once with a pid and a log \
                                file that `job_op` reads, waits for or kills.";

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
    run_env: &RunEnv,
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> Result<String> {
    Ok(run_command_detailed(config, run_env, command, cwd, timeout_secs)
        .await?
        .report)
}

/// Run a shell command, returning both the combined output and the exit code.
pub async fn run_command_detailed(
    config: &Config,
    run_env: &RunEnv,
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> Result<CommandOutcome> {
    run_command_streaming(config, run_env, command, cwd, timeout_secs, false).await
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
    run_env: &RunEnv,
    command: &str,
    cwd: &Path,
    timeout_secs: u64,
    idle_kill: bool,
) -> Result<CommandOutcome> {
    let run = shell_invocation(config, command);
    // No context is wrapped around the result: `killed after 120s` does not need to be
    // prefixed with the shell that was running it, and a spawn that fails already names
    // the program it could not start.
    run_program_streaming(config, run_env, &run, None, cwd, timeout_secs, idle_kill).await
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

/// Everything a command flint starts is handed about the run that started it.
///
/// One function for both spawn sites (the foreground runner and the background one), because the
/// two had already grown the same six-line proxy block and a fact that reaches one but not the
/// other is a fact a script cannot rely on. The proxy is first and unchanged: it is what makes a
/// command able to download on a network where direct access is blocked, and it was the only thing
/// a command was told before this.
fn apply_child_env(cmd: &mut tokio::process::Command, config: &Config, run_env: &RunEnv) {
    if let Some(proxy) = config.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        cmd.env("HTTP_PROXY", proxy);
        cmd.env("HTTPS_PROXY", proxy);
        cmd.env("http_proxy", proxy);
        cmd.env("https_proxy", proxy);
        cmd.env("ALL_PROXY", proxy);
        cmd.env("all_proxy", proxy);
    }
    run_env.apply(cmd);
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
    run_env: &RunEnv,
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
    apply_child_env(&mut cmd, config, run_env);

    // `kill_on_drop` as well as the explicit kill below. The explicit one is what
    // actually reaps the process; this is the backstop for every *other* way the future
    // can end -- an interrupt drops the turn, and a dropped turn must not leave a build
    // running behind it.
    // Before the spawn, and on the command itself: what the guard below signals has to be
    // decided at that moment, not after.
    KillTree::detach(cmd.as_std_mut());
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
/// On Unix `sh -c` usually *becomes* a lone command, so the process killed is the command -- but a
/// script that backgrounds work (`sh -c 'make & wait'`) leaves it running, because there is no
/// process group in play for anything to signal. That is the same gap as the Windows one and it is
/// closed the same way, mechanically: [`KillTree::detach`] puts the child in a group of its own
/// before it is spawned, and the guard signals the group rather than the process.
struct KillTree {
    #[cfg(any(unix, windows))]
    pid: Option<u32>,
}

impl KillTree {
    /// Put the child in a process group of its own, so the guard below can reach what it starts.
    ///
    /// This is the Unix half of `taskkill /T`: there is no tree to walk here, so the group is what
    /// makes the whole of it addressable with one signal. Called before `spawn`, which is the only
    /// moment `std` will accept it.
    ///
    /// A group of its own is not a lifetime of its own -- nothing about the child's exit changes,
    /// and a child that outlives its budget is still killed by the same guard as before. What
    /// changes is only *what the signal reaches*.
    #[cfg(unix)]
    fn detach(cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        // `0` asks for a new group whose id is this child's pid, so the id the guard already has
        // from `Child::id()` is also the group to signal -- no second handle to keep in step.
        cmd.process_group(0);
    }

    /// Windows needs nothing here: `taskkill /T` walks the tree from the live parent instead,
    /// which is why that path has the "not found" limitation its own comment records.
    #[cfg(not(unix))]
    fn detach(_cmd: &mut std::process::Command) {}

    fn arm(pid: Option<u32>) -> Self {
        #[cfg(any(unix, windows))]
        {
            KillTree { pid }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            KillTree {}
        }
    }

    /// The command ended by itself; leave whatever it started alone.
    fn defuse(&mut self) {
        #[cfg(any(unix, windows))]
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
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // The *group*, which is what `detach` made the pid stand for; a bare `kill` here
            // would be the bug this guard exists to close. `SIGKILL` and not a polite signal,
            // because this runs on the abnormal paths only -- a timeout, an idle kill, a turn
            // dropped by an interrupt -- where the question is whether the work stops, not
            // whether it stops tidily. Nothing to wait for: `SIGKILL` cannot be caught, and the
            // child is reaped by whoever was already waiting on it.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// Start one command and come back without waiting for it, with its output going to a file.
///
/// The same job record as a child's, because the facts are the same and only three of them differ: a
/// pid, a budget enforced whether or not anybody is watching, a handle to read later, and one notice
/// when it ends. What differs is where its output goes (a log file rather than a conversation), how
/// it is stopped (a kill rather than `/stop`), and the fact that there is nothing to wait for but the
/// file -- which is what `JobKind::Command` carries.
///
/// Two decisions are worth reading rather than skimming:
///
/// * **The output goes to a file, not into memory.** A command run this way is usually the long one
///   -- a build, an install, a test suite -- and its log is exactly the thing that is too big for a
///   request. The file is the answer, and it is plain text a person can `tail`.
/// * **It does not die with the call, but it should not outlive the run either.** `kill_on_drop` is
///   deliberately *not* set here, because the whole point is that this outlives the tool call that
///   started it. What ends it is its budget, a `stop`, or the run itself: dropping this task on the
///   way out of the process fires the guard below, which ends the tree -- `taskkill /T` on Windows
///   and the process group on Unix, in both cases including what the shell started rather than only
///   the shell.
// Eight arguments, and the lint is not wrong -- they are this job's own bookkeeping (its stdin, its
// log file, the label a person reads) plus the run's environment, and a struct holding them would
// exist only to satisfy the lint. Same call as the REPL's own signature in `main.rs`.
#[allow(clippy::too_many_arguments)]
async fn start_background_command(
    config: &Config,
    run_env: &RunEnv,
    run: &Invocation,
    stdin: Option<&str>,
    cwd: &Path,
    timeout_secs: u64,
    log: PathBuf,
    label: String,
) -> Result<std::sync::Arc<Job>> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let file =
        std::fs::File::create(&log).with_context(|| format!("cannot write {}", log.display()))?;

    let mut cmd = tokio::process::Command::new(&run.program);
    apply_invocation(cmd.as_std_mut(), run);
    cmd.current_dir(cwd);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    // Both streams into the one file, in the order they arrive: a build's warnings and its errors
    // belong in the order they happened, and this file is the record of what it did.
    cmd.stdout(Stdio::from(
        file.try_clone()
            .with_context(|| format!("cannot write {}", log.display()))?,
    ));
    cmd.stderr(Stdio::from(file));
    apply_child_env(&mut cmd, config, run_env);

    KillTree::detach(cmd.as_std_mut());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot spawn program '{}'", run.program))?;
    let pid = child.id().unwrap_or(0);

    // The caller's text goes in through the pipe, never through the command line -- and detached,
    // because a program that never reads its input must not hold up the handle.
    if let Some(text) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            let bytes = text.as_bytes().to_vec();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = pipe.write_all(&bytes).await;
                let _ = pipe.shutdown().await;
            });
        }
    }

    let job = std::sync::Arc::new(Job {
        kind: JobKind::Command,
        pid,
        label: label.clone(),
        prompt: label,
        started: JobMoment::now(),
        ended: std::sync::Mutex::new(None),
        // A command is not a run: it is not part of the depth chain, and `readonly` was decided by
        // the tool call that got here, before anything was spawned.
        depth: 0,
        readonly: false,
        // Always background: this function exists for the calls that said so.
        background: true,
        // A command has no conversation to keep or to name, whatever the run around it was told.
        no_session: false,
        reported: std::sync::atomic::AtomicBool::new(false),
        ended_by_us: std::sync::atomic::AtomicBool::new(false),
        stopping: std::sync::atomic::AtomicBool::new(false),
        timeout_secs,
        // Nothing is ever written to a command's stdin after it starts: the payload, when there is
        // one, has already gone in through the detached writer above.
        stdin: std::sync::Mutex::new(None),
        session: std::sync::Mutex::new(None),
        session_known: tokio::sync::Notify::new(),
        finished: std::sync::Mutex::new(None),
        done: tokio::sync::Notify::new(),
        schema_file: std::sync::Mutex::new(None),
        log: Some(log),
        read_offset: std::sync::Mutex::new(0),
    });
    forget_old_finished();
    jobs().push(std::sync::Arc::clone(&job));
    // The page is told the list changed, not what changed: `GET /jobs` is the list.
    jobs_changed();

    // The supervisor, for the same reason a child has one: a `timeout_secs` that only a waiter could
    // enforce would be a budget that never comes due for a job nobody is waiting for.
    let supervisor = std::sync::Arc::clone(&job);
    tokio::spawn(async move {
        let mut tree = KillTree::arm(Some(pid));
        let (code, note) = match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait(),
        )
        .await
        {
            Ok(status) => {
                tree.defuse();
                (
                    // A job this run ended reads as killed whatever the shell said, for the reason
                    // `ended_by_us` gives: the number belongs to the shell that was signalled, not to
                    // the command somebody stopped.
                    if supervisor.was_ended_here() {
                        -1
                    } else {
                        status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1)
                    },
                    String::new(),
                )
            }
            Err(_) => {
                // Out of budget. The guard fires *here* rather than at the end, for the reason its
                // own comment gives: `/T` walks the shell's children, and once the shell is gone
                // they are reparented and cannot be found any more.
                //
                // The budget ending a command is this run ending it too -- the same word, because the
                // fact a reader is looking for is "did it stop on its own or was it stopped", and the
                // *note* below is what says why.
                supervisor.ended_here();
                drop(tree);
                let _ = child.start_kill();
                let _ = child.wait().await;
                (
                    -1,
                    format!("\nkilled after {timeout_secs}s: the command's budget ran out\n"),
                )
            }
        };
        *supervisor
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Finished {
            code,
            note,
            // A command's complaints are in its log with everything else; there is no second stream
            // to keep apart.
            stderr: String::new(),
            collected: Collected::default(),
        });
        *supervisor
            .ended
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(JobMoment::now());
        supervisor.done.notify_waiters();
        // A job that ended is news to a page that is showing it as running, whether or not anybody
        // was waiting for it -- and the notice below goes to the *person's* screen, which is a
        // different reader from the page.
        jobs_changed();
        // The person's lane, exactly as for a child: a command started in the background is a job
        // nobody is waiting for, and a build that ended while they were reading is news.
        if !supervisor.was_reported() {
            notice(&supervisor.finished_notice());
        }
    });

    Ok(job)
}

/// What a `bash`/`exec`/`pwsh` call gets back when it does not wait: a handle, and what to do with it.
///
/// Synchronous, unlike a child's: there is no conversation to wait to be named, because the file the
/// command writes to is created before the command is spawned. That is also why the path is in the
/// handle at all -- it is the thing a person can read with `tail`, and the thing a model can be
/// pointed at when the verb is not what it wants.
fn command_handle(job: &Job) -> String {
    let log = job
        .log
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    format!(
        "started in the background: pid {} ({}). It is running and this call is not waiting for \
         it.\noutput: {log}\nUse `job_op` with action \"status\" to see where it is, \"output\" to read \
         what it has printed since you last asked, \"wait\" to collect all of it when it ends, or \
         \"stop\" to kill it (pid {}). Its budget is {}s, and nobody has to be waiting for that to \
         happen. You will be told when it ends even if you do not ask.",
        job.pid,
        util::preview(&job.label, 80),
        job.pid,
        job.timeout_secs
    )
}

/// The file a background command's output goes to.
///
/// In this session's own directory, where the spilled output and the `pwsh` scripts already go, and
/// named for the tool that started it as well as numbered: the three tools that can start a
/// background command keep their own counters, and `background-1.log` from two of them at once would
/// be one command's output disappearing into another's.
fn background_log_path(dir: &Path, tag: &str, n: usize) -> PathBuf {
    dir.join(format!("background-{tag}-{n}.log"))
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

    /// A background command's log goes where the spilled output goes, so that one directory holds
    /// everything this session wrote and a person can find the log by reading the handle.
    fn use_spill_dir(&mut self, dir: &Path) {
        self.spill_dir = dir.to_path_buf();
    }

    fn run_env(&mut self) -> Option<&mut RunEnv> {
        Some(&mut self.env)
    }

    fn description(&self) -> &str {
        "Run a shell command line. The working directory is kept between calls."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command line to execute." },
                "download": {
                    "type": "boolean",
                    "description": "Set when the command fetches something over the network. A \
                                    download is killed only if it stops producing output, not on \
                                    a total time budget."
                },
                "background": { "type": "boolean", "description": BACKGROUND_WAITS },
                "timeout_secs": { "type": "integer", "description": TIMEOUT_SECS }
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
        // Opt-in for a command, unlike `task` and deliberately the other way round: a command's
        // output is usually the input to the next step, so waiting is the useful default. What the
        // flag buys is the long build, and the model is the one that knows which one it is asking for.
        let background = optional_bool(args, "background")?;

        let timeout = match optional_u64(args, "timeout_secs")? {
            Some(explicit) => explicit,
            // A download is bounded by idleness rather than by a total budget: it may
            // legitimately take an hour, and what matters is whether it is still moving.
            None if download => DOWNLOAD_BASH_TIMEOUT,
            // A command somebody chose not to wait for is by definition not a two-minute command.
            None if background => LONG_BASH_TIMEOUT,
            // A command that installs or builds is expected to be slow, and killing
            // `cargo install` at two minutes is not a safety feature -- it is a false
            // alarm that teaches the model to work around the tool.
            None if looks_slow(command) => LONG_BASH_TIMEOUT,
            None => DEFAULT_BASH_TIMEOUT,
        }
        .clamp(1, 24 * 3600);

        if self.readonly && !is_readonly_command(command) {
            return Err(anyhow!(readonly_refusal(
                command,
                "Turn it off with /readonly, or use a read-only command."
            )));
        }

        // Reached only after the gate above, which is the same gate a foreground command goes
        // through: a background call is not a way around `readonly`.
        if background {
            let log = background_log_path(
                &self.spill_dir,
                "bash",
                self.logs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            );
            let run = shell_invocation(&self.config, command);
            let job = start_background_command(
                &self.config,
                &self.env,
                &run,
                None,
                &self.cwd,
                timeout,
                log,
                util::preview(command, 120),
            )
            .await?;
            return Ok(command_handle(&job));
        }

        let outcome =
            run_command_streaming(&self.config, &self.env, command, &self.cwd, timeout, download)
                .await?;
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
    /// What this command is told about the run that asked for it, filled in by
    /// [`ToolBox::with_run_env`].
    env: RunEnv,
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
            env: RunEnv::default(),
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

    fn run_env(&mut self) -> Option<&mut RunEnv> {
        Some(&mut self.env)
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
                        "Kill the script after this many seconds (default {DEFAULT_BASH_TIMEOUT}, or \
                         {LONG_BASH_TIMEOUT} when it runs in the background). Raise it for anything \
                         known to be slow."
                    )
                },
                "background": {
                    "type": "boolean",
                    "description": format!(
                        "Default false: wait here for the script. Set true for anything slow, and the \
                         call returns at once with a pid and a log file (default budget \
                         {LONG_BASH_TIMEOUT}s) for `job_op` to read, wait for or kill."
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
        let background = optional_bool(args, "background")?;
        let timeout = match optional_u64(args, "timeout_secs")? {
            Some(explicit) => explicit,
            None if background => LONG_BASH_TIMEOUT,
            None => DEFAULT_BASH_TIMEOUT,
        }
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

        if background {
            let log = background_log_path(
                &self.script_dir,
                "pwsh",
                self.scripts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            );
            let label = format!("pwsh {}", util::preview(script, 100));
            let job = start_background_command(
                &self.config,
                &self.env,
                &run,
                stdin,
                &self.cwd,
                timeout,
                log,
                label,
            )
            .await?;
            return Ok(command_handle(&job));
        }

        let outcome =
            run_program_streaming(&self.config, &self.env, &run, stdin, &self.cwd, timeout, false)
                .await?;

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
    /// Where a background command's log goes, and how many this session has started.
    spill_dir: PathBuf,
    logs: std::sync::atomic::AtomicUsize,
    /// What this command is told about the run that asked for it, filled in by
    /// [`ToolBox::with_run_env`].
    env: RunEnv,
}

#[async_trait::async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    /// A background command's log goes where the spilled output goes, for the reason `bash`'s does
    /// and one more: without this the log lands in the shared `spill/unattached/` directory, where
    /// every session's `background-exec-1.log` is the same file. Caught by a test that asserts the
    /// handle's path is inside this session's directory, not by reading the code.
    fn use_spill_dir(&mut self, dir: &Path) {
        self.spill_dir = dir.to_path_buf();
    }

    fn run_env(&mut self) -> Option<&mut RunEnv> {
        Some(&mut self.env)
    }

    fn description(&self) -> &str {
        "Run one program, arguments already separate. No shell."
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
                    "description": "One string per argument, in order, exactly as the program should receive it. No quoting and no escaping: [\"commit\", \"-m\", \"a message with spaces\"] is three arguments. Put a regex or a JSON body in `stdin`, or in a file whose path you pass."
                },
                "stdin": {
                    "type": "string",
                    "description": "Text to write to the program's standard input."
                },
                "timeout_secs": { "type": "integer", "description": TIMEOUT_SECS },
                "background": { "type": "boolean", "description": BACKGROUND_WAITS }
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
        let background = optional_bool(args, "background")?;

        // The time and download heuristics read the call as a line, which is the only
        // place an argument array is ever flattened. Nothing is executed from it: the
        // runner below gets `program` and `argv` exactly as they arrived.
        let label = label_of(program, &argv);
        let download = looks_like_download(&label);

        let timeout = match optional_u64(args, "timeout_secs")? {
            Some(explicit) => explicit,
            None if download => DOWNLOAD_BASH_TIMEOUT,
            None if background => LONG_BASH_TIMEOUT,
            None if looks_slow(&label) => LONG_BASH_TIMEOUT,
            None => DEFAULT_BASH_TIMEOUT,
        }
        .clamp(1, 24 * 3600);

        if self.readonly && !exec_is_readonly(program, &argv) {
            return Err(anyhow!(readonly_refusal(
                &label,
                "Turn it off with /readonly, or run a program that only inspects."
            )));
        }

        // No shell, and therefore no shell's quoting rules to satisfy: `exec` hands over an
        // argument list. A caller that wants shell syntax wants `bash`, whose invocation is
        // built by `shell_invocation`.
        let run = Invocation::plain(program, argv);

        // After the gate, as in `bash`: backgrounding is not a way around `readonly`.
        if background {
            let log = background_log_path(
                &self.spill_dir,
                "exec",
                self.logs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            );
            let job = start_background_command(
                &self.config,
                &self.env,
                &run,
                stdin,
                &self.cwd,
                timeout,
                log,
                util::preview(&label, 120),
            )
            .await?;
            return Ok(command_handle(&job));
        }

        let outcome =
            run_program_streaming(&self.config, &self.env, &run, stdin, &self.cwd, timeout, download)
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

/// The sentence a command gets when `readonly` refuses it, from whichever door asked.
///
/// One string for four doors -- the `bash` tool, the `exec` tool, the `!` line a person types, and
/// the `exec` subcommand -- because the rule they share is one rule, and a guard whose refusals read
/// differently by door is one whose *judgement* looks different by door too. That is not a style
/// preference: two of those doors used to skip the judgement entirely while a banner on the same
/// screen said "writes and mutating commands are refused" (measured, 2026-09-18), and a shared
/// sentence is the cheapest way for the four to be seen moving together.
///
/// The tail is the only part that differs, and only where it must: `/readonly` is a command inside a
/// session, and the subcommand never sees one.
pub fn readonly_refusal(label: &str, how_to_turn_it_off: &str) -> String {
    format!(
        "readonly mode is ON: refusing to run `{}`. {how_to_turn_it_off}",
        util::preview(label, 120)
    )
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
        "Read a text file, with line numbers."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "File to read: absolute, or relative to the working directory. `path` is an alias." },
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
        "Create or overwrite a text file."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "File to write. `path` is an alias." },
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
        "Replace an exact string in a file."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "File to edit. `path` is an alias." },
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
            //
            // The escapes are doubled on purpose: `\r` in a Rust literal is a carriage return,
            // and one inside a message moves the cursor to column 0 of the answer strip, so
            // the reader would see the sentence partly overwritten. The model needs the two
            // characters to type, not the byte.
            let hint = if text.contains("\r\n") && old.contains('\n') && !old.contains("\r\n") {
                " This file uses CRLF line endings: put \\r\\n between lines in old_string."
            } else if !text.contains("\r\n") && old.contains("\r\n") {
                " This file uses LF line endings: old_string has \\r\\n in it, which is not there."
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
        "Apply a patch to one or more text files, all or nothing. Use this instead of several `edit` \
         calls when one change spans files. Format: `*** Begin Patch`, then one section per file -- \
         `*** Add File: <path>`, `*** Update File: <path>` or `*** Delete File: <path>` -- ending with \
         `*** End Patch`. In an update, `+` adds a line, `-` removes one, a space is context, and `@@ \
         ... @@` starts another hunk. Each hunk is found by its own lines, so a file changed since it \
         was read fails the whole patch. Files must be read before being updated or deleted."
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

/// One entry of a directory, as every door that lists one describes it.
///
/// The `list` tool and the page's own listing (`GET /dir` in `src/web.rs`) are two doors onto one
/// answer, and an answer that differed between them would be exactly the drift this repository spends
/// comments preventing: a person reading the panel and a model reading a tool result would be told
/// different things about the same directory. So the shape, the size and the *order* live here; each
/// door decides only how to render it — the tool as the text a model reads, the route as the JSON the
/// page draws buttons from.
pub struct DirItem {
    pub name: String,
    /// Whether it is a directory. Follows a link, like everything else that asks this question here:
    /// a junction on Windows (`Application Data`) and a symlinked directory on Unix are directories
    /// to a person, and calling them files because of how they are stored is a distinction nobody
    /// asked for.
    pub dir: bool,
    /// The size for a file, and whatever the metadata says for a directory (which is a number on
    /// Windows and 4 KB on Unix). Printed for files only, because a directory's own size is a fact
    /// about the filesystem rather than about the directory.
    pub size: u64,
}

impl DirItem {
    /// The one line that names it, which is also the sort key.
    ///
    /// A directory carries a trailing `/` rather than a size: that is how every shell prints one, it
    /// is what tells a reader (and the page's scanner, which reads these lines as text) that this name
    /// is a place rather than a file, and it is why a directory's *name* can hold a space without
    /// becoming ambiguous in the one place flint can do something about it.
    pub fn line(&self) -> String {
        if self.dir {
            format!("{}/", self.name)
        } else {
            format!("{}  ({} bytes)", self.name, self.size)
        }
    }
}

/// A directory's entries, sorted the way both doors print them.
///
/// Sorted by the *printed line* rather than by name, which is what `list` has always done: it puts
/// `My Projects/` beside `My Notes.txt` instead of in a block of its own, and it means the order is a
/// property of what a reader sees rather than of a comparison somebody would have to keep in step with
/// the formatting. The cost is that the sort is not the one a shell's `dir` gives, which is the reason
/// this is stated where both callers can read it.
///
/// Synchronous, and called from an async tool body: a directory listing is one syscall per entry and
/// this process already reads files synchronously on both sides of the same runtime (`serve_file`,
/// `serve_open`, `jobs_snapshot`). One definition shared by both doors is worth more than an async
/// loop that only this tool would have.
pub fn directory_items(path: &Path) -> std::io::Result<Vec<DirItem>> {
    let mut items = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        // A name that vanished between the read and the stat is a file that was there a moment ago:
        // it is still an entry, and calling it zero bytes is the honest answer about it.
        let meta = entry.metadata().ok();
        items.push(DirItem {
            name: entry.file_name().to_string_lossy().to_string(),
            dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        });
    }
    items.sort_by_key(|a| a.line());
    Ok(items)
}

#[async_trait::async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn description(&self) -> &str {
        "List a directory."
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
        let items = directory_items(&path)
            .with_context(|| format!("cannot list {}", path.display()))?;
        if items.is_empty() {
            return Ok(format!("{} is empty", path.display()));
        }
        let lines: Vec<String> = items.iter().map(|item| item.line()).collect();
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
        // A model that writes `~/notes.txt` means the person's home directory, which is the file a
        // `read` has to open for the transcript's own path button to be reading the same one.
        assert_eq!(resolve_path(cwd, "~/notes.txt"), crate::config::home_dir().join("notes.txt"));
        // ...while a file named with a leading tilde is a name in the working directory, not a home.
        assert_eq!(resolve_path(cwd, "~notes.txt"), PathBuf::from("/work/~notes.txt"));
    }

    #[tokio::test]
    async fn bash_tool_refuses_mutation_in_readonly() {
        // The guard must live inside the tool, not only in the UI.
        let tool = BashTool {
            config: Config::default(),
            readonly: true,
            cwd: PathBuf::from("."),
            spill_dir: std::env::temp_dir(),
            logs: std::sync::atomic::AtomicUsize::new(0),
            env: RunEnv::default(),
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
        "Find files by name pattern."
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
        "Search file contents."
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

/// Everything a child run needs, resolved once: where it works, what endpoint it talks to, whether
/// it may write, and which agent profiles this directory offers.
///
/// Shared by the two tools that start children, because a fan-out is not a second feature. It is the
/// same child, started more than once, and two copies of this would be two places for "same as here"
/// to stop being true.
#[derive(Clone, Default)]
pub struct TaskConfig {
    /// Where the child works unless the call names another directory.
    cwd: PathBuf,
    /// Whether *this* run is readonly. A readonly run may not spawn a writing child: the flag is
    /// monotonic, or `readonly` would stop meaning anything the moment a model could call `task`.
    readonly: bool,
    /// The endpoint handed to the child, so that "same as here" stays true even when this run was
    /// started with `--provider`/`--model` flags that appear nowhere in the config.
    provider: String,
    model: String,
    /// What `.flint/agents/*.md` offered here, for a call that names one. Each carries the path it
    /// was found at, which is how its body is read when it is used -- the name is never turned back
    /// into a path.
    agents: Vec<context::AgentProfile>,
    /// The session this run is having, which is what its children are told started them.
    ///
    /// `None` for a run that keeps no conversation of its own (`--no-session`): a child told it came
    /// from nothing would be filed as though nobody had asked for it, so `no_session` says the same
    /// thing the other way round -- there is no parent to name, and no file to leave.
    parent: Option<String>,
    /// Whether this run keeps no conversation, which decides the same for the children it starts.
    ///
    /// Not a default and not a guess: a child is a conversation this run asked for, so a flag about
    /// what this run leaves on disk has to reach it, or `--no-session` would write nothing except
    /// through the one door it opened itself.
    no_session: bool,
}

/// A child that is ready to start.
struct Child {
    argv: Vec<String>,
    /// What it was asked, kept for the status listing: two children of one parent are told apart by
    /// their jobs, not by their pids.
    prompt: String,
    /// The depth it will run at, which is this run's depth plus one.
    depth: u32,
    /// The session that started it, passed as `FLINT_PARENT` rather than as an argument.
    ///
    /// Not an argument on purpose: the child's own record of where it came from is not something the
    /// model driving the call gets to choose or leave out, and a flag would also be visible to the
    /// child's model in its own command line -- `flint who` prints command lines.
    parent: Option<String>,
    timeout_secs: u64,
    readonly: bool,
    /// Whether this run started it without waiting. Carried on the child's own job record, because the
    /// decision outlives the call that made it: the notice at the end depends on it.
    background: bool,
    /// Whether the child was told to keep no conversation, which is what its own stream will say
    /// instead of naming a file. Carried so that a status line can say "none, and none was asked for"
    /// rather than "not named yet" -- the one answer that would stay wrong forever.
    no_session: bool,
    /// The profile that shaped it, if one did.
    agent: Option<String>,
    /// Where its job sits in a fan-out, for the header of its block.
    index: usize,
    /// The schema handed to it through a file, to delete when it is done.
    schema_file: Option<PathBuf>,
}

/// One call, several children, at the same time.
///
/// This is the shape "run N of these" actually has: not N model calls in one conversation, but N
/// conversations. Each child is a whole run with its own context, its own session file and its own
/// bill, and the answers come back labelled in the order they were asked for -- so a model reading
/// the result can tell which answer belongs to which question without parsing prose.
///
/// What this deliberately is not: a shared context. The children cannot see this conversation or
/// each other, so nothing is learned by one that helps another. If the jobs depend on each other,
/// they are not jobs for this tool, and running them anyway is how a fan-out turns into N confident
/// wrong answers.
pub struct TasksTool {
    config: TaskConfig,
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
    config: TaskConfig,
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
/// flint like this one" would quietly mean a different model. `no_session` is passed for the same
/// reason one step further out: "another flint like this one" includes what this one keeps, and a
/// `--no-session` run whose child wrote a conversation left exactly one file behind.
//
// Eight arguments, and the lint is not wrong: each one is a fact the child has to be *told* rather
// than left to resolve for itself. A struct would exist only to satisfy the lint -- the same call
// `main.rs` makes for the REPL's own signature -- and the test that holds this shape still asserts a
// *list* of argv, so it would be flattened straight back out there.
#[allow(clippy::too_many_arguments)]
pub fn task_argv(
    exe: &Path,
    prompt: &str,
    cwd: &Path,
    readonly: bool,
    provider: &str,
    model: &str,
    schema: Option<&Path>,
    no_session: bool,
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
    if no_session {
        argv.push("--no-session".to_string());
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
        // A signal, and the word the page's `status` has always used for it. It said `unknown` here,
        // so the same finished job was `killed` in one door and `(unknown)` in the other two
        // (`/jobs`, and the sentence `job_op` hands back) -- measured with `job_op stop`.
        -1 => "killed",
        65 => "the answer is not usable: a schema never matched, or the run ran out of steps",
        69 => "a person must act: no key, rejected credentials, or an account with no balance",
        75 => "the retries ran out; asking again later is right",
        130 => "the run was stopped",
        _ => "unknown",
    }
}

/// How many children one `tasks` call may start, and how many of them may run at once.
///
/// Eight in the list and four at a time: the limit is not about the machine, it is that every child
/// is a billed run. A model that wants twenty answers asks twice, and each answer it already has is
/// in front of it when it asks the second time.
pub const MAX_FAN_OUT: usize = 8;
const DEFAULT_PARALLEL: usize = 4;

/// The sentence to hand back when this run is already as deep as the chain goes.
///
/// A *result* rather than an error: "not started, and here is why" is an answer the model can act on,
/// while an error invites a retry that cannot succeed.
fn depth_refusal() -> Option<String> {
    let depth = task_depth();
    (depth + 1 > MAX_TASK_DEPTH).then(|| {
        format!(
            "not started: this is already a flint run at depth {depth}, and flint does not go \
             deeper than {MAX_TASK_DEPTH}. Do this work here, or ask for it in a run that was \
             started at the top."
        )
    })
}

/// The schema, handed to the child through a file: a schema inline has to survive a command line,
/// and a path always does. Named for this process and this job, because a fan-out prepares several
/// children inside the same nanosecond.
fn write_schema_file(args: &Value, index: usize) -> Result<Option<PathBuf>> {
    let Some(schema) = args.get("schema") else {
        return Ok(None);
    };
    if !schema.is_object() {
        return Err(anyhow!(
            "task `schema` must be a JSON Schema object, not {}",
            schema
        ));
    }
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "flint-task-schema-{}-{}-{}.json",
        std::process::id(),
        index,
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, serde_json::to_string(schema)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path))
}

/// Turn one call into a child that is ready to start, or say why it cannot be.
///
/// Everything that decides how a child runs is resolved here, in one place, so that `task` and
/// `tasks` cannot drift: the profile, the endpoint, the flag that cannot be loosened, the depth it
/// will run at, and the prompt it will be given.
fn prepare_child(config: &TaskConfig, args: &Value, prompt: &str, index: usize) -> Result<Child> {
    let cwd = match optional_str(args, "cwd")? {
        Some(dir) => {
            let path = resolve_path(&config.cwd, dir);
            if !path.is_dir() {
                return Err(anyhow!("task cwd is not a directory: {}", path.display()));
            }
            path
        }
        None => config.cwd.clone(),
    };

    // A profile is *defaults*, not orders: an argument on the call still wins -- except for
    // `readonly`, which a profile can only add. Otherwise a profile would be a way to talk a
    // readonly run into a writing child, which is the one property this whole path exists to keep.
    let agent = optional_str(args, "agent")?.map(str::to_string);
    let (body, profile_readonly, profile_model, profile_provider) = match &agent {
        Some(name) => {
            let profile = config
                .agents
                .iter()
                .find(|a| &a.name == name)
                .cloned()
                .ok_or_else(|| context::unknown_agent_error(name, &config.agents))?;
            (
                context::agent_body(&profile)?,
                profile.readonly,
                profile.model.clone(),
                profile.provider.clone(),
            )
        }
        None => (String::new(), false, None, None),
    };

    let requested_readonly = optional_bool(args, "readonly")?;
    let readonly = config.readonly || requested_readonly || profile_readonly;
    let provider = optional_str(args, "provider")?
        .map(str::to_string)
        .or(profile_provider)
        .unwrap_or_else(|| config.provider.clone());
    let model = optional_str(args, "model")?
        .map(str::to_string)
        .or(profile_model)
        .unwrap_or_else(|| config.model.clone());
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .filter(|secs| *secs > 0)
        .unwrap_or(600);

    // The profile's words come first and are separated from the job, because they say different
    // things: one is how to work, the other is what to do. The child has no flag for "system
    // prompt", so this is where a profile's instructions enter its conversation.
    let prompt_text = if body.is_empty() {
        prompt.to_string()
    } else {
        format!("{body}\n\n---\n\n{prompt}")
    };

    let schema_file = write_schema_file(args, index)?;
    let exe = match std::env::var("FLINT_BIN") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        // The same binary as this one: a child built from anything else would be a different
        // flint than the one the caller is talking to. `FLINT_BIN` is for a wrapper that wants
        // to point at a specific build, and for the tests, which live outside the binary.
        _ => std::env::current_exe().context("cannot find the flint binary to run")?,
    };
    let argv = task_argv(
        &exe,
        &prompt_text,
        &cwd,
        readonly,
        &provider,
        &model,
        schema_file.as_deref(),
        config.no_session,
    );
    Ok(Child {
        argv,
        prompt: prompt.to_string(),
        depth: task_depth() + 1,
        parent: config.parent.clone(),
        timeout_secs,
        readonly,
        no_session: config.no_session,
        // A fan-out collects every answer in one result, so its children are children somebody is
        // waiting for. `task` decides its own, from the call.
        background: false,
        agent,
        index,
        schema_file,
    })
}

#[async_trait::async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Ask another flint to do a job in its own context and give back its answer. Use it for work \
         that is large or self-contained -- a wide search, reading a lot of files, a question whose \
         transcript you do not want here. The child starts fresh, so `prompt` has to stand alone. \
         `agent` names a profile from this directory's `.flint/agents/`, which decides its \
         instructions, model and whether it may write. It returns at once with a handle; `background: \
         false` waits for the answer instead, for when what you do next depends on it."
    }

    fn task_config(&mut self) -> Option<&mut TaskConfig> {
        Some(&mut self.config)
    }

    fn schema(&self) -> Value {
        let mut properties = json!({
            "prompt": {
                "type": "string",
                "description": "What to ask."
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
                "description": "Stop the child after this many seconds (default 600); its half answer survives."
            },
            "background": {
                "type": "boolean",
                "description": "Default true: return at once with a handle. Set false to wait for the \
                     answer -- only when what you do next depends on it."
            }
        });
        // Only offered when this directory actually has profiles, and only the names that exist: an
        // `agent` property with nothing behind it is a schema's worth of tokens teaching the model
        // that a tool lies. The catalog is in the description, because a model chooses by reading it.
        if !self.config.agents.is_empty() {
            let names: Vec<&str> = self.config.agents.iter().map(|a| a.name.as_str()).collect();
            let catalog: Vec<String> = self.config.agents.iter().map(|a| a.summary()).collect();
            properties["agent"] = json!({
                "type": "string",
                "enum": names,
                "description": format!(
                    "A profile from this directory's .flint/agents/, which brings its own instructions, \
                     model and readonly. Available here: {}",
                    catalog.join("; ")
                )
            });
        }
        json!({
            "type": "object",
            "properties": properties,
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
        if let Some(refusal) = depth_refusal() {
            return Ok(refusal);
        }
        let mut child = prepare_child(&self.config, args, prompt, 0)?;
        // `background` is a *decision about waiting*, not about the child: the same run, the same
        // session, the same bill. What changes is that this call hands back a handle instead of the
        // answer, and the default is the handle.
        //
        // It did not used to be, and the measured cost of the old default is in this repository's
        // history: a person asked for a wide search, the model called `task`, and the parent session
        // was unusable until the child finished -- and typing to interrupt it (which is what a person
        // does next) drops the turn instead. A child is a whole run: its answer is being written to a
        // conversation of its own whether or not anybody waits, so waiting buys nothing the file does
        // not already hold. A model that needs the answer *now* says `background: false`, and that is
        // the door the old behaviour is still available through.
        let background = args
            .get("background")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        child.background = background;
        let job = start_child(child)?;
        if background {
            Ok(background_handle(job).await)
        } else {
            finish_child(job).await
        }
    }
}

/// What became of a child this run started: look at it, wait for it, or ask it to stop.
///
/// The other half of `task`, which now hands back a handle by default, and deliberately *one* tool
/// rather than three:
/// every tool schema is re-sent with every request of every conversation, so three verbs about one
/// child would cost three schemas to say one thing. It is deliberately not folded into `task`
/// either -- a call that sometimes returns an answer and sometimes a receipt is one whose result a
/// model has to guess at, and `background` is the only reason there would be to guess.
///
/// It is scoped to the children *this* run started, and that is not a shortcut: a process can only
/// write `/stop` to a stdin it holds, and only the run that spawned a child holds one. Another run's
/// child is visible through its presence record -- which is what `flint who` reads, and what the
/// session path in that record is for -- and is handled there by a person, not from here.
pub struct JobOpTool;

/// The tools whose arguments follow a convention, and which are therefore the ones worth declaring.
///
/// The split is not "common versus rare" -- it is **guessable versus not**. `read` takes a path and
/// `bash` takes a command line, and a model that has met a coding agent before gets those right
/// without being told; `apply_patch` takes a patch in a format nothing else uses, and `job_op` is
/// flint's own idea. Measured against a local model: with *nothing* declared but a catalogue, it
/// called `read` with `file_path` and got the right answer in the same number of turns as the eager
/// build -- and, asked for `apply_patch`'s argument, answered "`name`" (the real one is `patch`)
/// without looking it up.
///
/// So these eight are the ones worth declaring on every request, and the rest are behind `tools`.
/// That is about half the payload saved rather than nine tenths, and it buys the thing the
/// measurement says is missing: the tools a run reaches for constantly never depend on the model
/// choosing to look something up.
///
/// **Not the default.** `Config::eager_tools` is empty unless somebody asks for a set, so the
/// shipped shape is all-lazy: the lookup and its catalogue, 822 characters, and every tool obtained
/// by asking. This list is the conservative answer for a model that guesses instead of looking --
/// the eight whose arguments need no lookup, 4,355 characters of tool text on every request when
/// they are declared. Paste it into `eager_tools` when a model's refusals show it guessing.
///
/// Measured both ways with `deepseek-flash`, on four tasks including a real `apply_patch` edit:
/// identical results, and the lazy shape cost 1,401/2,093/1,885 prompt tokens against
/// 2,651/2,781/3,099. It also called `tools` before `apply_patch` and answered `patch` -- the
/// behaviour a local 9B model did not show, which is why this is a config key and not a constant.
pub const CONVENTIONAL_TOOLS: [&str; 8] = [
    "bash", "exec", "read", "write", "edit", "list", "glob", "grep",
];

/// Ask what you can do, and how to call it.
///
/// The one tool that is always in the request when `lazy_tools` is on, and the reason the other
/// twelve are not. Every schema costs its characters on **every** request, for the life of the
/// tool; thirteen of them measured 10,710, four times the system prompt. So the request carries
/// this one and a one-line catalogue of the rest, and a tool joins the request the moment the model
/// asks about it.
///
/// This is the pattern the skill catalogue already uses -- names and summaries always visible, the
/// body loaded only when it is wanted -- applied to tools. What makes it work for tools where a
/// plain catalogue would not is the *unlock*: a call to this tool with a name puts that tool in the
/// next request's `tools`, so the model reads the arguments and then calls it as it always would.
/// Without that, a model that knows what `read` does still could not call it, because a provider
/// only accepts a call to a tool the request declared.
///
/// **The catalogue is always visible, and that is deliberate.** The model can see that `apply_patch`
/// exists before deciding to use it, so "a tool that appears and disappears is one a model cannot
/// plan around" -- the argument written next to `task` -- does not apply: nothing disappears, and
/// what appears, appears because the model asked.
pub struct ToolsTool {
    /// One line per tool: its name and the first sentence of its description.
    catalogue: String,
    /// Shared with the `ToolBox`, which is what turns a request for a tool into a schema in the
    /// next request. The same shape `reads` uses: state one tool writes and another consults.
    unlocked: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

#[async_trait::async_trait]
impl Tool for ToolsTool {
    fn name(&self) -> &str {
        "tools"
    }

    fn description(&self) -> &str {
        &self.catalogue
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Which tool. Leave it out to list what there is."
                }
            },
            "required": []
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let wanted = args.get("name").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
        if wanted.is_empty() {
            return Ok(format!("{}\n\nCall this again with one of those names to get the arguments it takes.", self.catalogue));
        }
        let mut unlocked = self.unlocked.lock().unwrap_or_else(|e| e.into_inner());
        if !unlocked.insert(wanted.to_string()) {
            return Ok(format!("{wanted} is already in your list; call it with the arguments in the schema you were given."));
        }
        Ok(format!(
            "{wanted} is now in your list, from this turn on. Call it as usual -- its arguments are \
             in the schema that came with this answer."
        ))
    }
}

/// The few words that make a tool's name enough to choose by, or empty when the name is enough.
///
/// **This is the only tool text paid for on every request**, so it says no more than the choice
/// needs: a run picks `exec` over `bash` from these words alone. It is deliberately not a place for
/// detail -- a tool's own description is paid for only from the turn it is unlocked, and the schema
/// arrives with it, so putting the same words here would move them to the most expensive place in
/// the request to save the model nothing.
///
/// Written as an explicit table rather than derived from the descriptions, because a derived one
/// would be a sentence (which is what this exists to avoid) and because a new tool must not appear
/// in the catalogue by accident: `every_tool_has_a_gloss` fails until somebody decides what it
/// says. An empty gloss is a decision too.
fn gloss(name: &str) -> Option<&'static str> {
    Some(match name {
        "bash" => "run a shell command line",
        "exec" => "run a program, arguments already separate",
        "read" => "read a text file",
        "write" => "create or overwrite a file",
        "edit" => "replace an exact string in a file",
        "apply_patch" => "one patch across several files",
        "list" => "list a directory",
        "glob" => "find files by name pattern",
        "grep" => "search file contents",
        "fetch" => "read a web page",
        "task" => "ask another flint; hands back a handle",
        "tasks" => "several flint runs at once",
        "job_op" => "check, wait for or stop a job",
        "skill" => "load a skill's body",
        "search" => "search the web",
        "pwsh" => "run a PowerShell script",
        // The lookup itself is named in the sentence above the list; a gloss for it would be the
        // sentence again.
        "tools" => "",
        _ => return None,
    })
}

/// The one-line catalogue the `tools` tool carries: the tools this request does **not** declare.
///
/// Only those, because the rest are in the request already, with their full schemas -- naming them
/// here as well would be the duplication this whole design removes, paid for on every turn.
fn catalogue_of(tools: &[Box<dyn Tool>], lazy: bool, eager: &[String]) -> String {
    let mut names: Vec<String> = Vec::with_capacity(tools.len());
    for tool in tools {
        let name = tool.name();
        if !lazy || name == "tools" || eager.contains(&name.to_string()) {
            continue;
        }
        match gloss(name) {
            Some("") => names.push(name.to_string()),
            Some(words) => names.push(format!("{name} ({words})")),
            // A tool with no gloss at all is a programming error, and one a test catches before a
            // run does. The catalogue still names it, because a tool the model cannot see is worse
            // than one it cannot tell apart.
            None => names.push(name.to_string()),
        }
    }
    if names.is_empty() {
        return "Every tool there is already has its arguments in your list; nothing is held back."
            .to_string();
    }
    // The sentence about what a call *does* is here and not also beside the argument, because both
    // places are paid for on every request and saying it twice cost 75 characters a turn to say it
    // once. The argument description is what is left when the tool's own description has already
    // explained the mechanism.
    format!(
        "The tools you can call but have not been given the arguments for: {}. Call this with one \
         of those names to get what it takes -- it joins your list from the next turn, and you call \
         it as usual.",
        names.join(", ")
    )
}

/// What the presence records say about a run this one did not start.
fn foreign_run(pid: u32) -> Option<String> {
    let listing = crate::live::scan();
    let (record, state) = listing
        .alive
        .iter()
        .find(|record| record.pid == pid)
        .map(|record| (record, "still running"))
        .or_else(|| {
            listing
                .stale
                .iter()
                .find(|record| record.pid == pid)
                .map(|record| {
                    (
                        record,
                        "stale: its process is gone and nothing removed its record",
                    )
                })
        })?;
    let session = if record.session.is_empty() {
        "its session was never named".to_string()
    } else {
        format!("session {}", record.session)
    };
    Some(format!(
        "pid {pid} is a flint run this one did not start, and it is {state}: {}, {}/{}, readonly: {}, \
         last seen {}s ago, {session}. Only the run that started a child holds its stdin, so this one \
         can be seen rather than waited for or stopped -- `flint who` names it for a person.",
        record.cwd.display(),
        record.provider,
        record.model,
        record.readonly,
        crate::live::now_secs().saturating_sub(record.last_seen)
    ))
}

/// Which job an action is about: the pid when one was named, otherwise the only one it could mean.
///
/// "Job" rather than "child" throughout since background commands share this: a pid is a pid, and a
/// caller that names one should not have to remember which tool started it.
fn job_for(pid: Option<u32>, action: &str) -> Result<std::sync::Arc<Job>> {
    if let Some(pid) = pid {
        if let Some(job) = job_by_pid(pid) {
            return Ok(job);
        }
        return Err(anyhow!(
            "no job of this run has pid {pid}. {}",
            foreign_run(pid).unwrap_or_else(|| "Nothing on this machine says it is either: `flint \
                                                 who` lists the runs that exist, and `job_op` \
                                                 action \"status\" lists the jobs this run started."
                .to_string())
        ));
    }
    let running: Vec<std::sync::Arc<Job>> = jobs()
        .iter()
        .filter(|job| !job.has_finished())
        .cloned()
        .collect();
    // Nothing running: a finished job is still worth collecting, and there is only one thing a bare
    // `wait` could mean when exactly one of them exists.
    let candidates = if running.is_empty() {
        jobs_listed()
    } else {
        running
    };
    match candidates.len() {
        0 => Err(anyhow!(
            "this run has started no jobs, so there is nothing to {action}. `task` starts a child and \
             comes back with its pid at once, and `bash`/`exec`/`pwsh` start a background command the \
             same way with `background: true`."
        )),
        1 => Ok(candidates[0].clone()),
        _ => Err(anyhow!(
            "{} jobs of this run are in play: {} -- name one with `pid`.",
            candidates.len(),
            candidates
                .iter()
                .map(|job| job.pid.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Every job this run started, in the words whoever is asking reads it in.
///
/// Three doors onto one answer: `job_op` with `action: "status"` is the model's, `/jobs` typed at the
/// terminal is a person's, and the page's panel row is the same line sent as a report -- so the
/// listing a person sees and the listing the model is given cannot come to disagree about what is
/// running. One function rather than three, for the reason `docs/agents.md` gives about the handle
/// itself: two readers of one record should not be two implementations of it.
///
/// The sentence is written for a reader who is thinking ("`wait` collects what it said") rather than
/// for one who is scanning; the page's panel is where it is read as prose.
pub fn jobs_report(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => match job_by_pid(pid) {
            Some(job) => job.describe(),
            None => foreign_run(pid).unwrap_or_else(|| {
                format!(
                    "no job of this run has pid {pid}, and nothing on this machine says it is \
                     {pid}. `flint who` lists the runs that exist."
                )
            }),
        },
        None => {
            let listed = jobs_listed();
            if listed.is_empty() {
                "this run has started no jobs. `task` starts a child and comes back at once with its \
                 pid; it only waits when the call says `background: false`."
                    .to_string()
            } else {
                listed
                    .iter()
                    .map(|job| job.describe())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }
}

/// End a job this run started, and say what happened.
///
/// Two doors onto one behaviour, for the reason `jobs_report` gives: `job_op` with
/// `action: "stop"` is the model's, and `/jobs stop <pid>` typed at the terminal (or sent from the
/// page's own destructive row) is a person's. The two ends of it are deliberately different and the
/// difference is the point: a child is **asked** -- it is a run, and `/stop` lets it finish the
/// thought it was on -- while a command is **killed**, because there is nothing to ask. A person
/// stopping their own run's work cares about exactly that difference, so it is in the sentence they
/// get back rather than in a comment here.
///
/// `timeout_secs` is how long it has to go; the caller's default (20 s) is `job_op`'s, and the
/// terminal command passes none because a person is reading the answer rather than waiting on it --
/// both land on the same number in `stop_job` for the same reason the timeout has to come due
/// somewhere: the job's own budget is the last word either way.
pub async fn stop_job(pid: Option<u32>, timeout_secs: Option<u64>) -> Result<String> {
    let job = job_for(pid, "stop")?;
    if job.has_finished() {
        // Reading this line is reading the job: it has ended, so the report on the next request
        // would be news this call has just delivered.
        job.mark_reported();
        return Ok(format!(
            "it had already ended, so there was nothing to stop; `wait` collects what it {}.\n{}",
            match job.kind {
                JobKind::Child => "said",
                JobKind::Command => "printed",
            },
            job.describe()
        ));
    }
    let ended_how = match job.kind {
        JobKind::Child => {
            if !job.ask_to_stop().await {
                return Ok(format!(
                    "could not ask pid {} to stop: its stdin is closed, which means it has just \
                     ended.\n{}",
                    job.pid,
                    job.describe()
                ));
            }
            "asked to stop, and it did -- keeping the half of an answer it had drawn"
        }
        JobKind::Command => {
            job.kill();
            "killed it, and it is gone -- what it had already written is still in its log"
        }
    };
    let limit = timeout_secs.unwrap_or(20);
    match job.wait(Some(Duration::from_secs(limit))).await {
        Some(finished) => Ok(format!("{ended_how}.\n{}", job.render(&finished))),
        None => Ok(format!(
            "{}; it is still going {}s later. Its own budget of {}s ends it either way.\n{}",
            match job.kind {
                // A child that ignores `/stop` is a run choosing to finish the thought it was on,
                // and its own budget is what ends it.
                JobKind::Child => format!("asked pid {} to stop", job.pid),
                // A command that survives a `taskkill /T /F` is a kill that has not landed yet --
                // there is nothing for it to be deliberating about.
                JobKind::Command =>
                    format!("killed pid {}, and the kill has not landed yet", job.pid),
            },
            limit,
            job.timeout_secs,
            job.describe()
        )),
    }
}

#[async_trait::async_trait]
impl Tool for JobOpTool {
    fn name(&self) -> &str {
        "job_op"
    }

    fn description(&self) -> &str {
        "See what became of a job this run started and did not wait for -- a `task` child, or a command \
         started with `background: true`. Only jobs this run started can be waited for or stopped; a \
         run in another process is `flint who`'s business, not this tool's."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "output", "wait", "stop"],
                    "description": "status: where every job of this run is. output: what a background \
                                    command printed since you last asked. wait: the whole of what it \
                                    produced. stop: end it, keeping what it produced."
                },
                "pid": {
                    "type": "number",
                    "description": "Which job, as the handle gave it. Default: all of them for \
                                    `status`, the only one in play for the rest."
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "wait: give up after this long (default 300) and report where it is. stop: how long it has to stop (default 20)."
                }
            },
            "required": ["action"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let action = require_str(args, "action")?.trim().to_ascii_lowercase();
        let pid = args
            .get("pid")
            .and_then(|value| value.as_u64())
            .map(|pid| pid as u32);
        let seconds = args
            .get("timeout_secs")
            .and_then(|value| value.as_u64())
            .filter(|secs| *secs > 0);
        match action.as_str() {
            "status" => Ok(jobs_report(pid)),
            // A window on a running command, not a collection of it: the answer to "is it still
            // getting anywhere?" arrives while the job is still going, which is the whole reason
            // there is a file to read. Reading it does not consume the job, so the notice at the end
            // still comes -- except when the job has already ended, where reading to the end of the
            // file *is* collecting it.
            "output" => {
                let job = job_for(pid, "read the output of")?;
                Ok(job.new_output())
            }
            "wait" => {
                let job = job_for(pid, "wait for")?;
                let limit = seconds.unwrap_or(300);
                match job.wait(Some(Duration::from_secs(limit))).await {
                    Some(finished) => Ok(job.render(&finished)),
                    None => Ok(format!(
                        "still running after {limit}s (its own budget is {}s, and it is stopped then \
                         whatever anyone is waiting for).\n{}",
                        job.timeout_secs,
                        job.describe()
                    )),
                }
            }
            "stop" => stop_job(pid, seconds).await,
            other => Err(anyhow!(
                "job_op takes action \"status\", \"output\", \"wait\" or \"stop\", not {other:?}"
            )),
        }
    }
}

#[async_trait::async_trait]
impl Tool for TasksTool {
    fn name(&self) -> &str {
        "tasks"
    }

    fn description(&self) -> &str {
        "Run several flint runs at the same time, one per job, and give back every answer labelled in \
         the order asked. Use it when the jobs are genuinely independent -- the same question about N \
         things, or N options to try. The children cannot see this conversation or each other, so every \
         job stands alone. This spends N runs at once."
    }

    fn task_config(&mut self) -> Option<&mut TaskConfig> {
        Some(&mut self.config)
    }

    fn schema(&self) -> Value {
        let job = json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "What to ask this child; it stands alone." },
                "agent": { "type": "string", "description": "A .flint/agents/ profile for this child." },
                "readonly": { "type": "boolean", "description": "Refuse writes in this child." },
                "schema": { "type": "object", "description": "A JSON Schema for this child's answer." }
            },
            "required": ["prompt"]
        });
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_FAN_OUT,
                    "items": job,
                    "description": "The jobs, each complete on its own. They run at the same time and cannot see each other."
                },
                "cwd": { "type": "string", "description": "Directory for every child (default: this run's)." },
                "timeout_secs": { "type": "number", "description": "Stop each child after this long (default 600)." },
                "max_parallel": {
                    "type": "number",
                    "description": format!("How many may run at once (default {DEFAULT_PARALLEL}, at most {MAX_FAN_OUT}).")
                }
            },
            "required": ["tasks"]
        })
    }

    async fn call(&self, args: &Value) -> Result<String> {
        if let Some(refusal) = depth_refusal() {
            return Ok(refusal);
        }
        let jobs = args
            .get("tasks")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if jobs.is_empty() {
            return Err(anyhow!("tasks requires a non-empty `tasks` array"));
        }
        if jobs.len() > MAX_FAN_OUT {
            return Err(anyhow!(
                "tasks takes at most {MAX_FAN_OUT} jobs at once; {} were given. Ask twice.",
                jobs.len()
            ));
        }
        let max_parallel = args
            .get("max_parallel")
            .and_then(|value| value.as_u64())
            .map(|n| (n as usize).clamp(1, MAX_FAN_OUT))
            .unwrap_or(DEFAULT_PARALLEL);

        // Every child is prepared *before* any of them starts. A bad argument in job three must not
        // leave jobs one and two already spending money on work nobody is waiting for.
        let mut queue: std::collections::VecDeque<Child> = std::collections::VecDeque::new();
        for (position, job) in jobs.iter().enumerate() {
            let mut job = job.clone();
            if !job.is_object() {
                return Err(anyhow!("tasks[{position}] is not an object"));
            }
            // The call's own `cwd` and `timeout_secs` apply to every job that does not name its own.
            for key in ["cwd", "timeout_secs"] {
                if job.get(key).is_none() {
                    if let Some(value) = args.get(key) {
                        job[key] = value.clone();
                    }
                }
            }
            let prompt = job
                .get("prompt")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if prompt.trim().is_empty() {
                return Err(anyhow!("tasks[{position}] has no `prompt`"));
            }
            match prepare_child(&self.config, &job, &prompt, position + 1) {
                Ok(child) => queue.push_back(child),
                Err(error) => {
                    // Leave nothing behind: a schema written for a child that will not run is a file
                    // in the temp directory with nothing to delete it.
                    for child in &queue {
                        if let Some(path) = &child.schema_file {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                    return Err(error.context(format!("tasks[{position}]")));
                }
            }
        }

        let total = queue.len();
        let started = std::time::Instant::now();
        let mut blocks: Vec<(usize, Option<String>, String)> = Vec::new();
        while !queue.is_empty() {
            let batch: Vec<Child> = queue.drain(..max_parallel.min(queue.len())).collect();
            let mut handles = Vec::new();
            for child in batch {
                let index = child.index;
                let agent = child.agent.clone();
                // Started here and finished in a task below: `start_child` registers the child and
                // hands back the job, and every fan-out child runs in the background from the moment
                // it is spawned -- what makes this a batch is N jobs at once, not N waits at once.
                match start_child(child) {
                    Ok(job) => handles.push((index, agent, tokio::spawn(finish_child(job)))),
                    Err(error) => blocks.push((
                        index,
                        agent,
                        format!("the child could not be run: {error:#}"),
                    )),
                }
            }
            for (index, agent, handle) in handles {
                // A child that panicked is reported as a block of its own rather than dropped: an
                // answer that quietly lost one of three questions is worse than a visible failure.
                let text = match handle.await {
                    Ok(Ok(text)) => text,
                    Ok(Err(error)) => format!("the child could not be run: {error:#}"),
                    Err(error) => format!("the child did not finish: {error}"),
                };
                blocks.push((index, agent, text));
            }
        }
        blocks.sort_by_key(|(index, _, _)| *index);

        let mut out = format!(
            "{total} children ran, {max_parallel} at a time, {:.0}s in total. Each answer is \
             labelled with the job it belongs to.\n",
            started.elapsed().as_secs_f64()
        );
        for (index, agent, text) in blocks {
            out.push_str(&format!(
                "\n--- task {index}{} ---\n{text}\n",
                agent.map(|name| format!(" ({name})")).unwrap_or_default()
            ));
        }
        Ok(out)
    }
}

/// Start the child and hand back the job that follows it, without waiting for it.
///
/// This is the split that makes a background child possible at all: everything that has to happen
/// *while* the child runs -- draining its stream, learning its session, enforcing its timeout, noticing
/// that it ended -- belongs to detached tasks rather than to the caller, because the caller may be a
/// tool call that returned immediately or a turn that was dropped. `finish_child` is then only a wait
/// on a job that is already being watched.
///
/// The stream is read as it arrives rather than through `output()`, for one reason: a child that
/// has to be stopped should be stopped the way a person stops one -- `/stop` on its stdin, which
/// leaves the half-answer it had drawn in its session file -- and killing it is the fallback, not
/// the first move.
fn start_child(child: Child) -> Result<std::sync::Arc<Job>> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    // Before `argv` is moved out: the name this child is known by while it runs.
    let label = child.label();
    let prompt = child.prompt.clone();
    let argv = child.argv;
    let mut command = tokio::process::Command::new(&argv[0]);
    command.args(&argv[1..]).env("FLINT_DEPTH", child.depth.to_string());
    // Where the child's conversation came from. An environment variable rather than an argument, for
    // `FLINT_DEPTH`'s reason: the parent's id is not the model's to choose, and `flint who` prints the
    // command lines of runs on this machine -- a flag here would put one session's id in another
    // model's context for no reason. `main` reads it to name the parent in the child's `meta` line and
    // to file the child's session under `children/`, which is what keeps it out of the person's list.
    if let Some(parent) = &child.parent {
        command.env("FLINT_PARENT", parent);
    }
    let mut process = command
        // A child is not drawing on this terminal: its stdout is a pipe. The capture variables are
        // the debug build's way of pretending there *is* a terminal, and a child that inherits them
        // opens the same capture file as its parent and truncates it -- measured: the parent's
        // recording came back empty because the child had recreated the file under it. Removed here
        // rather than checked for at the other end, because the child has no way to know whose file
        // it would be opening.
        .env_remove("FLINT_TERM_CAPTURE")
        .env_remove("FLINT_TERM_CAPTURE_FILE")
        .env_remove("FLINT_TERM_SIZE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("running {}", argv[0]))?;

    // Said before the first frame can arrive: a child takes seconds to start up and make its first
    // request, and a status row that says the same word as before is the state this fixes.
    progress(&format!("{label}: starting"));

    let pid = process.id().unwrap_or(0);
    let stdin = process.stdin.take();
    let stdout = process.stdout.take().expect("stdout was piped");
    let stderr = process.stderr.take().expect("stderr was piped");

    // Registered before its stream is read, because the whole point is to survive this future being
    // dropped: after that, the detached reader below is the only thing still watching the child.
    let job = std::sync::Arc::new(Job {
        kind: JobKind::Child,
        pid,
        label: label.clone(),
        prompt,
        started: JobMoment::now(),
        ended: std::sync::Mutex::new(None),
        depth: child.depth,
        readonly: child.readonly,
        background: child.background,
        no_session: child.no_session,
        reported: std::sync::atomic::AtomicBool::new(false),
        ended_by_us: std::sync::atomic::AtomicBool::new(false),
        stopping: std::sync::atomic::AtomicBool::new(false),
        timeout_secs: child.timeout_secs,
        stdin: std::sync::Mutex::new(stdin),
        session: std::sync::Mutex::new(None),
        session_known: tokio::sync::Notify::new(),
        finished: std::sync::Mutex::new(None),
        done: tokio::sync::Notify::new(),
        schema_file: std::sync::Mutex::new(child.schema_file.clone()),
        // A child's output is its conversation, which it names for itself a moment from now.
        log: None,
        read_offset: std::sync::Mutex::new(0),
    });
    forget_old_finished();
    jobs().push(std::sync::Arc::clone(&job));
    // Registered is a change of the list the page draws: a job that exists but is not shown is the
    // one thing the page must not do, because a job nobody can see is a job nobody stops.
    jobs_changed();

    let reader_label = label.clone();
    let reader_job = std::sync::Arc::clone(&job);
    let reader = tokio::spawn(async move {
        let lines = tokio::io::BufReader::new(stdout).lines();
        let mut collected = Collected::default();
        let mut lines = Box::pin(lines);
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(note) = child_progress(&reader_label, &line) {
                progress(&note);
            }
            if let Some(path) = child_session(&line) {
                *reader_job
                    .session
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(path);
                reader_job.session_known.notify_waiters();
            }
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

    // The supervisor: the timeout, the graceful stop and the kill belong to the child rather than to
    // whoever is waiting for it, because a background child has nobody waiting -- an unwatched child
    // with a `timeout_secs` that only the waiter could enforce would run until the machine was
    // restarted, which is the opposite of what a timeout is for.
    let timeout_secs = child.timeout_secs;
    let supervisor_job = std::sync::Arc::clone(&job);
    tokio::spawn(async move {
        let (code, note) = supervise(&supervisor_job, &mut process, timeout_secs).await;
        // After the process ends, never before: a reader drained too early loses the last lines, and
        // the answer is written to the stream just before the exit.
        let collected = reader.await.unwrap_or_default();
        let stderr = complaints.await.unwrap_or_default();
        if let Some(path) = supervisor_job
            .schema_file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = std::fs::remove_file(path);
        }
        *supervisor_job
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Finished {
            code,
            note,
            stderr,
            collected,
        });
        *supervisor_job
            .ended
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(JobMoment::now());
        // Its stdout is closed and its exit status is known, so nothing is left running -- and this is
        // the one place that still runs when the parent stopped waiting: the tool's own future may
        // have been dropped long before, which is what a `task` child outliving its turn proved.
        supervisor_job.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
        supervisor_job.done.notify_waiters();
        // The page's list again: its row for this job has been saying "running" for as long as the
        // child took, and the exit code is the fact it is waiting for.
        jobs_changed();
        // Tell the person, whose session this is. Only for a job nobody was waiting for -- a child
        // waited on hands its answer over in the tool result, and the same news twice is noise -- and
        // only if nobody has read it: this lane and the report the model gets on its next request are
        // two readers of one fact, so neither marks the other's work done.
        if supervisor_job.background && !supervisor_job.was_reported() {
            notice(&supervisor_job.finished_notice());
        }
    });

    Ok(job)
}

/// Watch one child to the end: wait for it, stop it when its budget runs out, kill it if it will not go.
async fn supervise(
    job: &std::sync::Arc<Job>,
    process: &mut tokio::process::Child,
    timeout_secs: u64,
) -> (i32, String) {
    let stopped = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        process.wait(),
    )
    .await;
    match stopped {
        Ok(status) => (status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1), String::new()),
        Err(_) => {
            // Graceful first. `/stop` is the word the terminal takes, and the child keeps what it
            // had drawn; `kill` is only for a child that ignores it.
            job.ask_to_stop().await;
            match tokio::time::timeout(std::time::Duration::from_secs(20), process.wait()).await {
                Ok(status) => (
                    status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1),
                    format!("\nstopped after {timeout_secs}s: the child was told to stop\n"),
                ),
                Err(_) => {
                    let _ = process.kill().await;
                    let code = process
                        .wait()
                        .await
                        .map(|s| s.code().unwrap_or(-1))
                        .unwrap_or(-1);
                    (
                        code,
                        format!("\nkilled after {timeout_secs}s: the child did not stop when asked\n"),
                    )
                }
            }
        }
    }
}

/// Wait for a child that is already being watched, and hand back the one piece of text the model gets.
///
/// Takes the job by value so that a fan-out can spawn several of these at the same time: a borrow of
/// the tool would tie them all to one call frame.
async fn finish_child(job: std::sync::Arc<Job>) -> Result<String> {
    // The supervisor owns the real budget, and it always terminates: a wait that outlived it would
    // mean the supervisor task itself died, which is reported rather than waited on forever.
    let limit = Some(std::time::Duration::from_secs(job.timeout_secs + 60));
    match job.wait(limit).await {
        Some(finished) => Ok(job.render(&finished)),
        None => Ok(format!(
            "the child was not reaped: pid {} is still running after {}s.\n{}",
            job.pid,
            job.timeout_secs + 60,
            job.describe()
        )),
    }
}

/// What a `task` call gets back now that it does not wait: a handle, and what to do with it.
async fn background_handle(job: std::sync::Arc<Job>) -> String {
    // The child names its conversation before it asks the model anything, so this is a moment rather
    // than a guess -- and a handle that cannot say where the answer will land is not a handle. The
    // wait is bounded because a child that cannot start at all has nothing to name.
    let session = job.wait_session(std::time::Duration::from_secs(5)).await;
    let mut text = format!(
        "started in the background: pid {} ({label}). It is running and this call is not waiting for \
         it.\n",
        job.pid,
        label = job.label
    );
    match (&session, job.no_session) {
        (Some(path), _) => text.push_str(&format!("session: {path}\n")),
        // The same distinction a status line makes: a child told to keep nothing has no session
        // *coming*, and "not named yet" would send its caller back to ask again forever.
        (None, true) => text.push_str("session: none -- it was started with --no-session\n"),
        (None, false) => {
            text.push_str("session: not named yet -- ask `job_op` with action \"status\" for it\n")
        }
    }
    text.push_str(&format!(
        "Use `job_op` with action \"status\" to see where it is, \"wait\" to collect its answer \
         (pid {}), or \"stop\" to ask it to stop. Its budget is {}s, and nobody has to be waiting for \
         that to happen. You will be told when it ends even if you do not ask.\n\
         depth: {} (this run is {}), readonly: {}",
        job.pid, job.timeout_secs, job.depth, job.depth - 1, job.readonly
    ));
    text
}

/// How a child is named on the parent's status row while it runs.
///
/// The profile it was given, when the call named one, because that is what the person asked for and
/// what the model will see in the result. Otherwise the tool's own name, numbered when it is one of
/// several: a fan-out that shows one line for three children says less than nothing about which of
/// them is slow.
impl Child {
    fn label(&self) -> String {
        match (&self.agent, self.index) {
            (Some(agent), 0) => agent.clone(),
            (Some(agent), n) => format!("{agent} {}", n + 1),
            (None, 0) => "task".to_string(),
            (None, n) => format!("children {}", n + 1),
        }
    }
}

/// The phrase a child's own frame contributes to the parent's status row, when it has one.
///
/// Two frames, and both are about *now*: `tool.started` is what the child is doing, and `status` is
/// its heartbeat -- the only thing that arrives while it waits on a model, and the difference between
/// a child that is thinking and one that is stuck. Its answer is deliberately not here: deltas are
/// fragments of prose, and a status row that scrolls a child's answer past the parent's transcript is
/// not progress, it is a second transcript.
///
/// Measured, because the alternative was assumed and wrong: a parent's row said one unchanging word
/// (`task`) for the whole of a two-minute research child, and the person watching it could not tell
/// whether anything was happening -- which is the question a status row exists to answer.
fn child_progress(label: &str, line: &str) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let doing = match event.get("type").and_then(|v| v.as_str())? {
        "tool.started" => format!("running {}", event.get("name").and_then(|v| v.as_str())?),
        "status" => event.get("text").and_then(|v| v.as_str())?.to_string(),
        _ => return None,
    };
    Some(format!("{label}: {doing}"))
}

/// The session path a child's stream named, if this line was the one that named it.
fn child_session(line: &str) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if event.get("type").and_then(|v| v.as_str())? != "session.started" {
        return None;
    }
    event.get("session").and_then(|v| v.as_str()).map(str::to_string)
}

/// The frames worth keeping from a child's stream, gathered in one place.
#[derive(Default, Clone)]
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

    /// What a child's frames say on the parent's status row, and what they do not.
    #[test]
    fn a_childs_frames_are_reduced_to_what_it_is_doing_right_now() {
        assert_eq!(
            child_progress("task", r#"{"type":"tool.started","id":"c1","name":"search"}"#).as_deref(),
            Some("task: running search")
        );
        assert_eq!(
            child_progress(
                "child 2",
                r#"{"type":"status","text":"waiting for the model","elapsed_secs":5}"#
            )
            .as_deref(),
            Some("child 2: waiting for the model")
        );
        // The answer is not progress: a fragment of prose on the status row would be replaced by the
        // next fragment, so the child's answer would scroll past in pieces nobody can read.
        assert_eq!(
            child_progress("task", r#"{"type":"message.delta","text":"the answer "}"#),
            None
        );
        assert_eq!(child_progress("task", "not json at all"), None);
        // Where the answer will land, which is the fact that outlives the wait for it.
        assert_eq!(
            child_session(r#"{"type":"session.started","session":"C:\\s\\x.jsonl"}"#).as_deref(),
            Some(r"C:\s\x.jsonl")
        );
        assert_eq!(child_session(r#"{"type":"message.delta","text":"x"}"#), None);
    }

    /// The sentence about a job left running, in all four shapes -- and its punctuation.
    ///
    /// Held here because two of the four are hard to reach on purpose. A child that has not named its
    /// conversation is a *race*, not a feature: it happens when the machine is loaded enough that the
    /// child has not reached its first write when the turn is dropped, which is how the doubled full
    /// stop below was found. A test that talked to the real thing could only catch these shapes on a
    /// busy machine, so the words are asserted from the facts they are built out of.
    #[test]
    fn where_a_job_left_running_says_its_answer_is() {
        // The named child: a path to read, and the reason not to ask again. A plain string rather
        // than a raw one, because the line continuation is the point: the sentence is one line, and
        // asserting it as one is what catches a stray newline inside it.
        assert_eq!(
            answer_location(&JobKind::Child, Some(r"C:\h\sessions\x\children\1.jsonl"), None),
            "Its answer is being written to its own session, \
             C:\\h\\sessions\\x\\children\\1.jsonl -- read it there rather than asking for the same \
             work again."
        );
        // The child that has not said yet: no path, but a verb that answers for it later. A dead end
        // here is read as "nothing to collect", and the whole sentence exists to prevent that.
        let unnamed = answer_location(&JobKind::Child, None, None);
        assert!(unnamed.contains("`job_op`"), "{unnamed}");
        assert!(unnamed.contains("action: \"status\""), "{unnamed}");
        // A command's answer is where its output is going, which is the one thing a child does not
        // have -- and both kinds end in exactly one full stop, never two.
        for (kind, session, log) in [
            (
                JobKind::Command,
                None,
                Some(Path::new("C:/h/spill/s/background-bash-1.log")),
            ),
            (JobKind::Command, None, None),
            (JobKind::Child, Some("C:/h/child.jsonl"), None),
            (JobKind::Child, None, None),
        ] {
            let line = answer_location(&kind, session, log);
            assert!(line.ends_with('.'), "{line}");
            assert!(!line.ends_with(".."), "{line}");
        }
    }

    /// The child's command line is the whole interface between two flints, so its shape is held
    /// still here rather than only observed end to end.
    #[test]
    fn the_child_command_line_says_exactly_what_the_child_should_be() {
        let exe = Path::new("/usr/local/bin/flint");
        let cwd = Path::new("/work");
        let plain = task_argv(
            exe,
            "look around",
            cwd,
            false,
            "deepseek",
            "deepseek-chat",
            None,
            false,
        );
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
            false,
        );
        assert!(guarded.contains(&"--readonly".to_string()));
        assert_eq!(guarded.last().unwrap(), "/tmp/s.json");

        // An endpoint flint does not know is left unspoken rather than passed as an empty string,
        // which the child would take as a provider named "".
        let bare = task_argv(exe, "p", cwd, false, "", "", None, false);
        assert!(!bare.iter().any(|a| a == "--provider"));
        assert!(!bare.iter().any(|a| a == "--model"));
        assert!(!bare.iter().any(|a| a == "--readonly"));
        assert!(!bare.iter().any(|a| a == "--no-session"));

        // A run that keeps no conversation says so to the child it starts: the child is a
        // conversation this run asked for, so it is the one file `--no-session` could still leave.
        let keeping_nothing = task_argv(exe, "p", cwd, false, "p", "m", None, true);
        assert!(keeping_nothing.contains(&"--no-session".to_string()));
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
            .all_specs()
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
        // The two characters to type, not the bytes: a real `\r` inside a message moves the
        // cursor to column 0 of the answer strip and overwrites what is already on the line.
        assert!(
            message.contains("\\r\\n"),
            "the hint must spell the escape out: {message:?}"
        );
        assert!(
            !message.contains('\r') && !message.contains('\n'),
            "a line-ending hint is one line of text: {message:?}"
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

/// What a command is told about the run that started it.
///
/// A `RunEnv` is not interesting in itself; what has to hold is that it reaches a command a model
/// asked for *and* that a run with nothing to say says nothing -- the second half being the one a
/// process started by another run's command would otherwise get wrong, since a child inherits its
/// parent's environment whether or not anybody meant it to.
#[cfg(test)]
mod run_env_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn toolbox(dir: &TempDir) -> ToolBox {
        ToolBox::new(&Config::default(), false, dir.path().to_path_buf())
            .with_spill_dir(dir.path().join("spill"))
    }

    /// The three names, and the one case that must be a *removal*.
    ///
    /// Asserted on the command's own environment rather than through a shell: what is being decided
    /// here is what flint hands over, and a test that ran a program to find out would be measuring
    /// the shell as much as the decision. Each name is set to a stale value *first*, because
    /// "removed" and "never mentioned" read the same from the outside once the child is running --
    /// the command inherits this process's environment either way -- and only one of them is right.
    #[test]
    fn the_run_is_written_onto_the_command_and_a_missing_session_is_removed() {
        let mut cmd = tokio::process::Command::new("unused");
        for name in ["FLINT_SESSION", "FLINT_PROVIDER", "FLINT_MODEL"] {
            cmd.env(name, "stale");
        }
        let session = PathBuf::from("a-conversation.jsonl");
        RunEnv {
            session: Some(session.clone()),
            provider: "deepseek".to_string(),
            model: "deepseek-chat".to_string(),
        }
        .apply(&mut cmd);
        assert_eq!(
            value_in(&cmd, "FLINT_SESSION"),
            Some(session.display().to_string()),
            "the command was not told which conversation it is in"
        );
        assert_eq!(value_in(&cmd, "FLINT_PROVIDER").as_deref(), Some("deepseek"));
        assert_eq!(
            value_in(&cmd, "FLINT_MODEL").as_deref(),
            Some("deepseek-chat")
        );

        // A run with no conversation of its own, and no endpoint of its own to name: every name is
        // taken away rather than left as it arrived.
        let mut cmd = tokio::process::Command::new("unused");
        for name in ["FLINT_SESSION", "FLINT_PROVIDER", "FLINT_MODEL"] {
            cmd.env(name, "stale");
        }
        RunEnv::default().apply(&mut cmd);
        for name in ["FLINT_SESSION", "FLINT_PROVIDER", "FLINT_MODEL"] {
            assert_eq!(value_in(&cmd, name), None, "{name} arrived from somewhere else");
            assert!(
                mentions(&cmd, name),
                "{name} must be removed rather than simply not added"
            );
        }
    }

    /// `None` for removed, `Some("")` for set-but-empty; the removal is what is being read for.
    fn value_in(cmd: &tokio::process::Command, name: &str) -> Option<String> {
        cmd.as_std()
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().to_string())
    }

    /// Whether the command says anything about this name at all -- which a removal does, and an
    /// untouched inheritance does not.
    fn mentions(cmd: &tokio::process::Command, name: &str) -> bool {
        cmd.as_std().get_envs().any(|(key, _)| key == name)
    }

    /// The wiring, through the tool a model actually calls: a real shell, and the values it reads.
    ///
    /// This is the half the unit test above cannot see -- that `with_run_env` reaches the tool, and
    /// that the tool reaches the spawn.
    #[tokio::test]
    async fn a_command_the_bash_tool_runs_can_read_the_run_it_is_in() {
        let dir = TempDir::new("run-env");
        let session = dir.path().join("a-conversation.jsonl");
        let tools = toolbox(&dir).with_run_env(Some(session.clone()), "stub", "stub-model");

        let command = if cfg!(windows) {
            "echo %FLINT_SESSION%& echo %FLINT_PROVIDER%& echo %FLINT_MODEL%"
        } else {
            "echo \"$FLINT_SESSION\"; echo \"$FLINT_PROVIDER\"; echo \"$FLINT_MODEL\""
        };
        let out = tools
            .invoke("bash", &json!({ "command": command }))
            .await
            .expect("bash");

        assert!(
            out.contains(&session.display().to_string()),
            "the command was not told which conversation it is in: {out}"
        );
        assert!(
            out.contains("stub-model"),
            "the command was not told which model is paying: {out}"
        );
        // The provider name, checked on a whole line because it is a prefix of the model name.
        assert!(
            out.lines().any(|line| line.trim() == "stub"),
            "the command was not told which provider is paying: {out}"
        );

        // And the record kept for the caller outside the tool set -- the REPL's `!` escape -- is the
        // same one the tools were given, rather than a second copy that could drift.
        assert_eq!(tools.run_env().session.as_deref(), Some(session.as_path()));
        assert_eq!(tools.run_env().provider, "stub");
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

    /// A run with no conversation still spills into a directory that is its own.
    ///
    /// The name is the whole content of the claim, so the test asserts the name rather than the
    /// existence: `unattached` alone is shared by every session-less run on the machine, and these
    /// files are numbered from 1, so two `--no-session` runs at once would write over each other's
    /// `1.txt` -- the second one's bytes read out by the first one's model, which is a wrong answer
    /// nothing in the transcript would explain. The pid is what makes them two directories.
    #[test]
    fn a_run_with_no_conversation_spills_into_a_directory_of_its_own() {
        let expected = format!("unattached-{}", std::process::id());
        let dir = unattached_spill_dir();
        assert_eq!(
            dir.file_name().and_then(|name| name.to_str()),
            Some(expected.as_str()),
            "the spill directory of a session-less run does not name the process: {}",
            dir.display()
        );
        assert_eq!(
            dir,
            crate::config::spill_dir().join(&expected),
            "the spill directory is not under this home's spill root: {}",
            dir.display()
        );
    }
}

#[cfg(test)]
mod background_command_tests {
    use super::test_support::TempDir;
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    /// A command that takes a few seconds and says something before and after, so there is
    /// something to read *while* it runs. `ping` rather than `timeout`/`sleep`: this has to run on
    /// Windows, where neither exists, and the loopback ping is the shortest reliable wait there.
    fn slow_command() -> &'static str {
        if cfg!(windows) {
            "echo one & ping -n 4 127.0.0.1 >NUL & echo two"
        } else {
            "echo one; sleep 3; echo two"
        }
    }

    /// The pid and log file out of a handle, which is what a caller has to work with.
    fn handle_of(text: &str) -> (u32, PathBuf) {
        let pid = text
            .split("pid ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|pid| pid.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
            .unwrap_or_else(|| panic!("no pid in the handle: {text}"));
        let log = text
            .lines()
            .find_map(|line| line.strip_prefix("output: "))
            .unwrap_or_else(|| panic!("no output file in the handle: {text}"));
        (pid, PathBuf::from(log))
    }

    fn toolbox(dir: &TempDir, readonly: bool) -> ToolBox {
        ToolBox::new(&Config::default(), readonly, dir.path().to_path_buf())
            .with_spill_dir(dir.path().join("spill"))
    }

    /// Poll `job_op` action "output" until the wanted text shows up, the way a caller with a loop
    /// would. Not a sleep-and-hope: the point is that reading during a run is possible at all.
    async fn read_until(tools: &ToolBox, pid: u32, wanted: &str, tries: u32) -> String {
        let mut seen = String::new();
        for _ in 0..tries {
            seen.push_str(
                &tools
                    .invoke("job_op", &json!({ "action": "output", "pid": pid }))
                    .await
                    .expect("output"),
            );
            if seen.contains(wanted) {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        seen
    }

    /// The whole point of the feature: the call comes back before the command does, it says how to
    /// reach the answer, and the file it names is a file that actually exists.
    #[tokio::test]
    async fn a_background_command_hands_back_a_handle_at_once_and_names_a_log_that_exists() {
        let dir = TempDir::new("bg-handle");
        let tools = toolbox(&dir, false);
        let started = std::time::Instant::now();
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true }),
            )
            .await
            .expect("background bash");
        let took = started.elapsed();

        assert!(
            took < std::time::Duration::from_secs(2),
            "the call waited for the command: {took:?}"
        );
        let (pid, log) = handle_of(&handle);
        assert!(handle.contains("job_op"), "no way to reach it: {handle}");
        assert!(pid > 0, "no pid in the handle: {handle}");
        assert!(log.is_file(), "the named log does not exist: {log:?}");

        // Leave nothing running behind the test.
        let _ = tools
            .invoke("job_op", &json!({ "action": "stop", "pid": pid }))
            .await;
    }

    /// A person's stop is the same stop the model gets, and a job that was stopped reads as stopped.
    ///
    /// The first half is the refactor this came with: `job_op stop` and `/jobs stop <pid>` are one
    /// function, so the answer a person reads and the answer a model is given cannot drift. The
    /// second half is a defect the e2e test found on Windows and this one holds where it can be seen
    /// cheaply: a killed `cmd.exe` reports exit code 1, which the list would call `failed` -- the one
    /// word that sends somebody looking for a bug that is not there. On Unix the signal already
    /// reports -1, so this assertion is the Windows half of the fact; the flag it rests on is set
    /// where the decision is made, which is what makes the answer independent of the platform.
    #[tokio::test]
    async fn a_job_a_person_stops_reads_as_stopped_and_says_what_the_model_would_be_told() {
        // Longer than any test would wait: what is measured is that the stop arrives.
        let long = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >NUL & echo never"
        } else {
            "sleep 30; echo never"
        };
        let dir = TempDir::new("jobs-stop-person");
        let tools = toolbox(&dir, false);
        let handle = tools
            .invoke("bash", &json!({ "command": long, "background": true }))
            .await
            .expect("background bash");
        let (pid, _log) = handle_of(&handle);

        let said = stop_job(Some(pid), None).await.expect("a person's stop");
        assert!(
            said.contains("killed it, and it is gone"),
            "a person stopping a command is told what happened to it: {said}"
        );

        let row = job_row(pid);
        assert_eq!(
            row["status"], "killed",
            "a job somebody stopped is not a job that failed: {row}"
        );
        assert!(
            row["detail"].as_str().unwrap_or_default().contains("exit code -1"),
            "and the sentence beside it says which: {row}"
        );

        // The third door: the listing `/jobs` prints is the listing `job_op status` answers with, so
        // a person and a model reading the same run at the same moment cannot be told two things.
        let via_tool = tools
            .invoke("job_op", &json!({ "action": "status", "pid": pid }))
            .await
            .expect("status");
        assert_eq!(
            via_tool.trim(),
            jobs_report(Some(pid)).trim(),
            "the terminal's listing and the model's are two answers now"
        );
        assert!(
            jobs_report(None).contains(&format!("pid {pid}")),
            "the whole-run listing has to name it: {}",
            jobs_report(None)
        );
    }

    /// A job that has been asked to stop says so, for as long as that is true.
    ///
    /// The window is real and it is the one a listing is most likely to be read in: somebody who has
    /// just asked for a stop looks at the list to see whether it worked. `running` is the wrong word
    /// there -- it is doing nothing new, it is on its way out -- and `ended` is a promise the run
    /// cannot make yet, so the row says `stopping` until it is neither.
    ///
    /// The first half of this is deterministic by construction rather than by timing: `kill` is
    /// synchronous and the test does not await between the signal and the read, so on the test's own
    /// runtime the supervisor has had no chance to record the exit. The second half is the rule the
    /// flag must obey: the word is read *against* liveness, so a job that is gone is never described
    /// as stopping after the fact -- which is the failure a flag read on its own would produce.
    #[tokio::test]
    async fn a_job_asked_to_stop_says_stopping_until_it_is_gone() {
        let dir = TempDir::new("jobs-stopping");
        let tools = toolbox(&dir, false);
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true }),
            )
            .await
            .expect("background bash");
        let (pid, _log) = handle_of(&handle);
        let job = job_by_pid(pid).expect("the job just started");

        let before = job.describe();
        assert!(
            before.contains("-- running for"),
            "a job nobody has touched is running: {before}"
        );

        job.kill();
        let during = job.describe();
        // The state slot rather than the bare word: the line also carries a path, and this fixture's
        // directory is named after the state under test -- which is a false positive nobody enjoys
        // diagnosing twice.
        assert!(
            during.contains("-- stopping for"),
            "a job this run has just killed is not listed as on its way out: {during}"
        );
        assert!(
            !during.contains("exit code"),
            "the row claims it has ended before the supervisor could have seen it: {during}"
        );

        // What the model and the person read, not just what the struct would say: one listing, so the
        // word reaches whoever looks next through either door.
        assert!(
            jobs_report(Some(pid)).contains("-- stopping for"),
            "the listing a door prints does not carry the word: {}",
            jobs_report(Some(pid))
        );
        // And the page's row, which is the third reader of the same record: it is drawn exactly when
        // somebody is watching a stop they just asked for.
        let row = job_row(pid);
        assert_eq!(
            row["status"], "stopping",
            "the panel's row does not say it is on its way out: {row}"
        );

        let _ = job.wait(Some(std::time::Duration::from_secs(30))).await;
        let after = job.describe();
        assert!(
            after.contains("exit code"),
            "a stopped job does not say it ended: {after}"
        );
        assert!(
            !after.contains("-- stopping for"),
            "the word outlived the state it describes: {after}"
        );
    }

    /// The three words a row can carry, from the exit code and from who ended it. Pure, because the
    /// mapping is the whole of the decision: a job that was killed and a job that failed look the same
    /// from the outside, and a person reading the list is asking which one it was.
    ///
    /// The second argument is the half that was missing: a child this run asked to stop ends with
    /// **130** -- "the run was stopped", its own honest code -- and read from the code alone that is
    /// `failed`, which is the word `docs/features.md` §8 promises a kill never gets.
    #[test]
    fn a_jobs_state_word_is_its_exit_code_read_the_way_a_person_reads_it() {
        assert_eq!(job_status(0, false), "completed");
        assert_eq!(job_status(-1, false), "killed");
        assert_eq!(job_status(-1, true), "killed");
        assert_eq!(job_status(1, false), "failed");
        assert_eq!(job_status(130, false), "failed");
        assert_eq!(
            job_status(130, true),
            "killed",
            "a child this run stopped read as a failure"
        );
        assert_eq!(
            job_status(0, true),
            "completed",
            "a job that finished before the signal landed did not fail"
        );
    }

    /// The words in brackets after an exit code, which is the other door onto the same judgement.
    ///
    /// `-1` is a signal, and the table said `unknown` for it while `job_status` said `killed` for the
    /// same code -- so a command this run stopped was `killed` on the page and `(unknown)` in `/jobs`
    /// and in the sentence `job_op` handed back. Measured on a real `job_op stop`, 2026-09-18.
    #[test]
    fn the_words_beside_an_exit_code_agree_with_the_state_word() {
        assert_eq!(task_exit_meaning(-1), "killed");
        assert_eq!(task_exit_meaning(0), "finished");
        // The rich meanings stay rich where there is one: a code with something to say is not
        // flattened into a state word just because the two share a function now.
        assert_eq!(task_exit_meaning(130), "the run was stopped");
        assert_eq!(task_exit_meaning(69), "a person must act: no key, rejected credentials, or an account with no balance");
    }

    /// What the page's list is drawn from, for a job that is still running and then for the same job
    /// after it ends. The row has to be the *same* record both times -- one fact later -- and the
    /// clock has to be absolute, because the page computes an age from it rather than counting.
    #[tokio::test]
    async fn a_job_is_snapshotted_while_it_runs_and_again_when_it_has_ended() {
        let dir = TempDir::new("jobs-snapshot");
        let tools = toolbox(&dir, false);
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true }),
            )
            .await
            .expect("background bash");
        let (pid, log) = handle_of(&handle);

        let running = job_row(pid);
        assert_eq!(running["kind"], "command", "a command, not a child: {running}");
        assert_eq!(running["status"], "running", "{running}");
        assert_eq!(
            running["label"],
            slow_command(),
            "the row says what was asked, which for a command is its command line: {running}"
        );
        assert_eq!(running["detail"], "", "a running job has no ending to report");
        assert_eq!(
            running["path"],
            log.display().to_string(),
            "the row has to open the log the handle named: {running}"
        );
        let started = running["started_secs"].as_u64().expect("a start time");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            started >= before && started <= now,
            "the start is not on the wall clock: {started} not in {before}..={now}"
        );
        assert!(running["ended_secs"].is_null(), "it has not ended: {running}");

        tools
            .invoke("job_op", &json!({ "action": "wait", "pid": pid, "timeout_secs": 30 }))
            .await
            .expect("wait");

        let ended = job_row(pid);
        assert_eq!(ended["status"], "completed", "{ended}");
        assert_eq!(ended["pid"], running["pid"], "the same job, not a new one");
        assert_eq!(ended["started_secs"], running["started_secs"], "the same start");
        assert!(
            ended["detail"].as_str().unwrap_or_default().contains("exit code 0"),
            "the fact sentence has the code in it: {ended}"
        );
        assert!(
            ended["ended_secs"].as_u64().unwrap_or(0) >= started,
            "it ended before it started: {ended}"
        );
    }

    /// The same job's start, looked at twice: one number, because it is a fact recorded when the job
    /// started rather than a subtraction done at each look.
    ///
    /// This is the test that made the fix. `started_secs` used to be `now - elapsed`, which looks
    /// equivalent and is not: both ends are truncated to whole seconds, so where the fractional part
    /// of the clock happens to fall decides which second comes back, and a page polling one running
    /// job could be told it started at two different times a second apart. Windows CI found it once,
    /// which is the worst way to find out, so here it is on purpose: sample a running job across more
    /// than the second the derivation can be wrong about and demand one answer.
    #[tokio::test]
    async fn a_jobs_start_does_not_move_between_snapshots() {
        let dir = TempDir::new("jobs-start");
        let tools = toolbox(&dir, false);
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true }),
            )
            .await
            .expect("background bash");
        let (pid, _) = handle_of(&handle);

        // `slow_command` runs for three seconds and the derivation is wrong about one of them, so
        // 25 samples a tenth of a second apart cover the whole of what it can be wrong about. The
        // loop is counted rather than "until it ends" on purpose: a job that never ended would
        // otherwise hang this test rather than fail it.
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..25 {
            seen.insert(
                job_row(pid)["started_secs"]
                    .as_u64()
                    .expect("a start time"),
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(seen.len(), 1, "a running job's start moved while it ran: {seen:?}");
    }

    /// One job's row out of the snapshot, by pid. A helper rather than an index, because the list is
    /// the whole process's and another test running beside this one may have started a job of its
    /// own -- which is the same reason the page shows every job rather than only the newest.
    fn job_row(pid: u32) -> serde_json::Value {
        let snapshot = jobs_snapshot();
        snapshot["jobs"]
            .as_array()
            .expect("an array of jobs")
            .iter()
            .find(|job| job["pid"] == pid)
            .unwrap_or_else(|| panic!("no row for pid {pid} in {snapshot}"))
            .clone()
    }

    /// Reading while it runs, then collecting the whole of it: two different questions, and the
    /// second one has to answer with the first part as well.
    #[tokio::test]
    async fn a_running_commands_output_is_read_incrementally_and_wait_hands_over_all_of_it() {
        let dir = TempDir::new("bg-output");
        let tools = toolbox(&dir, false);
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true }),
            )
            .await
            .expect("background bash");
        assert!(
            handle.contains("pid "),
            "not a handle, so the call waited for the command: {handle}"
        );
        let (pid, _) = handle_of(&handle);

        let during = read_until(&tools, pid, "one", 40).await;
        assert!(
            during.contains("one"),
            "nothing was readable while it ran: {during:?}"
        );

        let whole = tools
            .invoke("job_op", &json!({ "action": "wait", "pid": pid, "timeout_secs": 30 }))
            .await
            .expect("wait");
        assert!(whole.contains("one"), "the early output is missing: {whole}");
        assert!(whole.contains("two"), "the late output is missing: {whole}");
        assert!(whole.contains("exit code: 0"), "no exit code: {whole}");

        // Incremental, not a replay: what `output` already handed over does not come back, which is
        // the difference between watching a build and being sent the same log on every poll.
        let after = tools
            .invoke("job_op", &json!({ "action": "output", "pid": pid }))
            .await
            .expect("output after the end");
        assert!(
            !after.contains("one"),
            "the same output was handed over twice: {after:?}"
        );
    }

    /// A kill has to leave the log readable: the half of the output that was written before the
    /// stop is the only thing there is, and throwing it away would make the verb useless.
    #[tokio::test]
    async fn a_background_command_can_be_stopped_and_keeps_what_it_had_written() {
        let dir = TempDir::new("bg-stop");
        let tools = toolbox(&dir, false);
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true }),
            )
            .await
            .expect("background bash");
        assert!(
            handle.contains("pid "),
            "not a handle, so the call waited for the command: {handle}"
        );
        let (pid, log) = handle_of(&handle);

        // Wait for the first line so there is something to keep.
        let during = read_until(&tools, pid, "one", 40).await;
        assert!(during.contains("one"), "nothing to keep: {during:?}");

        let stopped = tools
            .invoke("job_op", &json!({ "action": "stop", "pid": pid, "timeout_secs": 20 }))
            .await
            .expect("stop");
        assert!(
            stopped.contains("one"),
            "the stop threw away what it had written: {stopped}"
        );
        let kept = std::fs::read_to_string(&log).expect("the log survived");
        assert!(kept.contains("one"), "the log lost its first line: {kept:?}");
    }

    /// Which directory the log lands in is part of the handle being usable: a person reading the
    /// transcript follows the path into *this session's* directory, and a tool that was never told
    /// where that is falls back to the shared `unattached` one instead. That is the difference
    /// between a log a person finds and a log two sessions overwrite.
    #[tokio::test]
    async fn a_background_exec_writes_its_log_where_the_session_keeps_its_files() {
        let dir = TempDir::new("bg-exec");
        let tools = toolbox(&dir, false);
        let config = Config::default();
        // The same slow command as the bash tests, handed to `exec` as a program and an argument
        // list -- which is the other tool that can start a background command, and the one that had
        // to be told about the spill directory separately.
        let run = shell_invocation(&config, slow_command());
        let handle = tools
            .invoke(
                "exec",
                &json!({ "program": run.program, "args": run.args, "background": true }),
            )
            .await
            .expect("background exec");
        assert!(
            handle.contains("pid "),
            "not a handle, so the call waited for the command: {handle}"
        );
        let (pid, log) = handle_of(&handle);
        assert!(
            log.starts_with(dir.path().join("spill")),
            "the log went somewhere other than this session's directory: {log:?}"
        );

        let _ = tools
            .invoke("job_op", &json!({ "action": "stop", "pid": pid }))
            .await;
    }

    /// The budget belongs to the job, not to the waiter: this command is stopped by its own
    /// supervisor one second in, while nothing at all is waiting for it.
    #[tokio::test]
    async fn a_background_command_is_stopped_when_its_own_budget_runs_out() {
        let dir = TempDir::new("bg-budget");
        let tools = toolbox(&dir, false);
        let handle = tools
            .invoke(
                "bash",
                &json!({ "command": slow_command(), "background": true, "timeout_secs": 1 }),
            )
            .await
            .expect("background bash");
        assert!(
            handle.contains("pid "),
            "not a handle, so the call waited for the command: {handle}"
        );
        let (pid, _) = handle_of(&handle);

        let waited = tools
            .invoke("job_op", &json!({ "action": "wait", "pid": pid, "timeout_secs": 10 }))
            .await
            .expect("wait");
        assert!(
            waited.contains("budget ran out"),
            "the budget was not enforced while nobody waited: {waited}"
        );
    }

    /// The gate is not a foreground-only gate. A background call is the same permission question
    /// asked of the same command, and it must be answered before anything is spawned.
    #[tokio::test]
    async fn a_readonly_run_refuses_a_mutating_command_in_the_background_too() {
        let dir = TempDir::new("bg-readonly");
        let tools = toolbox(&dir, true);
        let refused = tools
            .invoke(
                "bash",
                &json!({ "command": "echo written > out.txt", "background": true }),
            )
            .await
            .expect_err("a readonly run allowed a mutating background command");
        assert!(
            format!("{refused}").contains("readonly"),
            "the refusal does not say why: {refused}"
        );
        assert!(
            !dir.path().join("out.txt").exists(),
            "the command ran anyway"
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

    /// The shipped shape: the first request declares the lookup and nothing else.
    ///
    /// Measured with `deepseek-flash` on four tasks, including a real `apply_patch` edit: identical
    /// results to the shape that declares eight tools, at 1,401/2,093/1,885 prompt tokens against
    /// 2,651/2,781/3,099. It calls `tools` before a tool whose arguments it cannot guess, and calls
    /// a conventional one straight away.
    #[test]
    fn the_first_request_declares_only_the_lookup() {
        let dir = TempDir::new("lazy-first");
        let box_ = toolbox(dir.path(), false);
        let names: Vec<String> = box_.specs().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names, vec!["tools".to_string()], "the first request declares: {names:?}");
        assert!(
            box_.all_specs().len() > 10,
            "the set is only small because it is empty: {} tools were built",
            box_.all_specs().len()
        );
    }

    /// And `eager_tools` is the other answer, for a model that guesses instead of looking.
    ///
    /// A local 9B model did exactly that: asked for `apply_patch`'s argument it answered "`name`"
    /// (the real one is `patch`) without asking. `CONVENTIONAL_TOOLS` is the list to paste for it.
    #[test]
    fn a_configured_eager_list_is_declared_and_the_rest_are_not() {
        let dir = TempDir::new("lazy-eager");
        let config = Config {
            lazy_tools: true,
            eager_tools: Some(CONVENTIONAL_TOOLS.iter().map(|n| n.to_string()).collect()),
            ..Config::default()
        };
        let box_ = ToolBox::new(&config, false, dir.path().to_path_buf());
        let names: Vec<String> = box_.specs().into_iter().map(|(n, _, _)| n).collect();
        for core in CONVENTIONAL_TOOLS {
            assert!(names.contains(&core.to_string()), "{core} is not declared: {names:?}");
        }
        assert!(names.contains(&"tools".to_string()), "the lookup is not declared: {names:?}");
        for held in ["apply_patch", "task", "tasks", "job_op", "fetch"] {
            assert!(
                !names.contains(&held.to_string()),
                "{held} is one a model gets wrong, and it is declared: {names:?}"
            );
        }
    }

    /// Every name in `CONVENTIONAL_TOOLS` is a tool, or the list a person pastes is a lie.
    #[test]
    fn the_conventional_list_names_real_tools() {
        let dir = TempDir::new("lazy-conventional");
        let box_ = toolbox(dir.path(), false);
        let all: Vec<String> = box_.all_specs().into_iter().map(|(n, _, _)| n).collect();
        for name in CONVENTIONAL_TOOLS {
            assert!(all.contains(&name.to_string()), "CONVENTIONAL_TOOLS names {name}, which is not a tool");
        }
    }

    /// A call that was never given its arguments is refused *and told where they are*.
    ///
    /// This is the repair loop for the failure the measurement found: the model guesses an argument
    /// rather than looking the tool up, and the refusal is the one moment it is willing to listen.
    #[tokio::test]
    async fn a_call_that_was_never_given_its_arguments_is_pointed_at_the_lookup() {
        let dir = TempDir::new("lazy-teach");
        let box_ = toolbox(dir.path(), false);
        let error = box_
            .invoke("apply_patch", &json!({ "name": "whatever" }))
            .await
            .expect_err("the wrong argument must fail");
        let text = format!("{error:#}");
        assert!(
            text.contains("tools") && text.contains("apply_patch"),
            "the refusal does not say where the arguments are: {text}"
        );

        // And once it has been asked about, the same mistake is told plainly -- the model has the
        // schema in front of it at that point, so a pointer would be noise.
        box_.invoke("tools", &json!({ "name": "apply_patch" })).await.unwrap();
        let error = box_
            .invoke("apply_patch", &json!({ "name": "whatever" }))
            .await
            .expect_err("the wrong argument still fails");
        let text = format!("{error:#}");
        assert!(
            !text.contains("not in your list yet"),
            "the pointer is repeated after the tool was declared: {text}"
        );
    }

    /// A call that failed at the *work* is not told where the schema is.
    ///
    /// The pointer is for a model that guessed its arguments. A call that had every argument it
    /// needed and failed on the content is not that, and Windows CI is where the difference showed:
    /// the CRLF hint is deliberately one line of text, and the pointer the first version appended
    /// made it three -- so `an_edit_against_crlf_says_so_when_the_text_does_not_match` failed, which
    /// is what a test holding a shape is for.
    #[tokio::test]
    async fn a_call_that_failed_at_the_work_is_not_told_about_the_lookup() {
        let dir = TempDir::new("lazy-work-failure");
        let box_ = toolbox(dir.path(), false);
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\n").unwrap();
        // `read` first, because the edit gate refuses a file this run has not read -- and that
        // refusal would be a different one than the test is about.
        box_.invoke("read", &json!({ "path": "f.txt" })).await.unwrap();

        let error = box_
            .invoke(
                "edit",
                &json!({ "path": "f.txt", "old_string": "not in the file", "new_string": "x" }),
            )
            .await
            .expect_err("the text is not there to replace");
        let text = format!("{error:#}");
        assert!(
            !text.contains("not in your list yet"),
            "a call that had its arguments was told to go and read them: {text}"
        );
        assert!(
            !text.contains('\n'),
            "the refusal must stay one line, which is what the CRLF case holds: {text:?}"
        );
    }

    /// A name that is not a tool at all is told what there is.
    #[tokio::test]
    async fn an_unknown_tool_is_told_what_there_is() {
        let dir = TempDir::new("lazy-unknown");
        let box_ = toolbox(dir.path(), false);
        let error = box_
            .invoke("definitely_not_a_tool", &json!({}))
            .await
            .expect_err("an unknown name must fail");
        let text = format!("{error:#}");
        assert!(text.contains("tools"), "the refusal does not point at the lookup: {text}");
        assert!(
            text.contains("apply_patch"),
            "the refusal does not say what is held back: {text}"
        );
    }

    /// Asking about a tool puts it in the next request, which is what makes lazy work at all.
    ///
    /// A provider only accepts a call to a tool the request declared. So the lookup answering with
    /// prose would leave the model knowing what `read` takes and still unable to call it -- the
    /// unlock is the half that matters.
    #[tokio::test]
    async fn asking_about_a_tool_adds_it_to_the_next_request() {
        let dir = TempDir::new("lazy-unlock");
        let box_ = toolbox(dir.path(), false);

        let answer = box_
            .invoke("tools", &json!({ "name": "read" }))
            .await
            .expect("the lookup answers");
        assert!(answer.contains("read"), "the answer must name it: {answer}");

        let names: Vec<String> = box_.specs().into_iter().map(|(n, _, _)| n).collect();
        assert!(names.contains(&"read".to_string()), "read was not unlocked: {names:?}");
        assert!(names.contains(&"tools".to_string()), "the lookup left the list: {names:?}");
        assert!(
            !names.contains(&"apply_patch".to_string()),
            "asking about one tool unlocked another: {names:?}"
        );
    }

    /// Asking with no name lists what there is, and unlocks nothing.
    #[tokio::test]
    async fn the_lookup_with_no_name_lists_the_tools() {
        let dir = TempDir::new("lazy-list");
        let box_ = toolbox(dir.path(), false);
        let answer = box_.invoke("tools", &json!({})).await.expect("the lookup answers");
        for (name, _, _) in box_.all_specs() {
            if box_.is_declared(&name) {
                continue;
            }
            assert!(answer.contains(&name), "the listing omits {name}: {answer}");
        }
        let before: Vec<String> = box_.specs().into_iter().map(|(n, _, _)| n).collect();
        let after: Vec<String> = box_.specs().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(before, after, "listing unlocked something");
    }

    /// Every tool is named in the catalogue's table, so a new one cannot appear without a decision.
    ///
    /// The catalogue is the only tool text paid for on every request; a tool that reached it with
    /// no words, or with a sentence, would be either a tool the model cannot choose between or the
    /// cost this design exists to remove.
    #[test]
    fn every_tool_has_a_gloss() {
        let dir = TempDir::new("lazy-gloss");
        let box_ = toolbox(dir.path(), false);
        for (name, _, _) in box_.all_specs() {
            let words = gloss(&name);
            assert!(words.is_some(), "{name} has no gloss in `gloss()`, so the catalogue cannot name it");
            let words = words.unwrap();
            assert!(
                words.len() <= 45,
                "{name}'s gloss is {} characters ({words:?}); the catalogue is paid for on every \
                 request, and the rest belongs in what the lookup answers with",
                words.len()
            );
        }
    }

    /// And the catalogue itself stays small, since that is what the whole design buys.
    #[test]
    fn the_catalogue_stays_small() {
        let dir = TempDir::new("lazy-catalogue");
        let box_ = toolbox(dir.path(), false);
        let catalogue = box_
            .all_specs()
            .iter()
            .find(|(n, _, _)| n == "tools")
            .map(|(_, d, _)| d.clone())
            .expect("the lookup is in the set");
        assert!(
            catalogue.len() <= 700,
            "the catalogue is {} characters: {catalogue}",
            catalogue.len()
        );
        // Every gloss is a phrase, not a sentence: a full stop inside the parentheses would mean a
        // description had been pasted into the catalogue, which is the cost this design removes.
        let listed = catalogue.split("The tools you can call: ").nth(1).unwrap_or("");
        let listed = listed.split(". Call this").next().unwrap_or("");
        assert!(
            !listed.contains(". "),
            "the catalogue is carrying sentences, which is what the lookup is for: {catalogue}"
        );
    }

    /// Every character of every schema is paid for on **every request**, for the life of the
    /// tool, so the total is a budget rather than an accident.
    ///
    /// Counted as name + description + parameters for every tool -- what the request body carries
    /// for the tools, without the per-tool envelope around it (measured at 10,710 with that
    /// included). Thirteen tools came to 13,523 before the trim, against a system prompt of 3,273:
    /// the tool payload was **four times** the prompt that everybody blamed for being long. It is
    /// 822 characters per request now, and 10,043 when a run sends everything.
    ///
    /// The box carries a search credential on purpose -- being *offered* costs no network -- so
    /// these are what a run with a DeepSeek provider pays rather than the floor a bare config sets.
    /// A skill directory would add one more catalogue line; nothing else here is optional.
    ///
    /// The ceilings are deliberately close, so they fail on a paragraph rather than on a rewrite:
    /// about a hundred and fifty characters of new prose trips the per-request or the eager one,
    /// and about two hundred and fifty trips the whole set. Raising one is allowed; raising it
    /// without deciding that the words are worth paying for on every request is not.
    #[test]
    fn the_tool_payload_stays_within_its_budget() {
        let dir = TempDir::new("schema-budget");
        // The three numbers are one config apart or none, so the messages below can quote what
        // this test measures instead of a figure somebody worked out by hand once -- which is how
        // the Windows delta came to be recorded as 880 when `pwsh` alone is 1,702.
        let search_ready = || Config {
            providers: vec![crate::config::ProviderConfig {
                name: "deepseek".to_string(),
                base_url: "https://api.deepseek.com/v1".to_string(),
                api_key: "x".to_string(),
                model: "deepseek-chat".to_string(),
                models: Vec::new(),
                api_key_env: None,
                start: None,
                stop: None,
                start_timeout_secs: 0,
                proxy: None,
                thinking_field: String::new(),
            }],
            ..Config::default()
        };
        let box_ = ToolBox::new(&search_ready(), false, dir.path().to_path_buf());
        let weight = |specs: Vec<(String, String, Value)>| -> usize {
            specs
                .iter()
                .map(|(name, description, schema)| {
                    name.len() + description.len() + serde_json::to_string(schema).unwrap().len()
                })
                .sum()
        };
        // The shipped shape: the lookup and its catalogue, and nothing else. This is everything a
        // turn pays until the model asks for more, so it is the number to hold closest.
        let per_request = weight(box_.specs());
        assert!(
            per_request <= 1_000,
            "one request carries {per_request} characters of tool text; it was 822 when this \
             budget was set, and 854 on Windows, where the catalogue carries one more line for \
             `pwsh`. The lookup and its catalogue are the only tool text paid for on every turn \
             of every run, so what grows here grows everywhere -- a paragraph here costs more than \
             a paragraph anywhere else in this file. Room for one more tool's line, and not for a \
             sentence: sentences are what the lookup answers with."
        );
        // And the other shape a person can ask for: the conventional eight declared on every
        // request whatever the lookup says. A different request, so its own number -- and this is
        // the box `eager_tools = CONVENTIONAL_TOOLS` really builds, which is the thing to measure:
        // when the default flipped to all-lazy, the old ceiling stayed behind on a box that no
        // longer held that list, so the shape a person pastes was guarded by nothing.
        let eager_box = ToolBox::new(
            &Config {
                eager_tools: Some(CONVENTIONAL_TOOLS.iter().map(|n| n.to_string()).collect()),
                ..search_ready()
            },
            false,
            dir.path().to_path_buf(),
        );
        let eager_request = weight(eager_box.specs());
        assert!(
            eager_request <= 4_500,
            "the eager shape carries {eager_request} characters of tool text; it was 4,355 when \
             this budget was set, and 4,387 on Windows. Every one of those schemas is there \
             because a model's refusals showed it guessing rather than looking, and all of it is \
             paid on every turn. Take a tool out of `CONVENTIONAL_TOOLS`, or trim one, before \
             raising the number."
        );
        // And everything, for the runs that ask for all of it -- the number this started at.
        // Only paid with `lazy_tools = false`, which is why this ceiling is the loosest: it is the
        // whole set, including the lookup, and it is nobody's per-turn cost by default.
        //
        // Per platform, because Windows carries `pwsh` and nothing else does: a 1,702-character
        // schema plus its 32-character catalogue line is 1,734 that the other two platforms never
        // pay. One ceiling would either fail on Windows or be slack everywhere else.
        let ceiling = if cfg!(windows) { 12_000 } else { 10_300 };
        let total = weight(box_.all_specs());
        assert!(
            total <= ceiling,
            "the whole tool set is {total} characters; it was 10,043 when the budget was set, and \
             11,777 on Windows (which is why the ceiling is per platform). \
             A run pays this on every turn when `lazy_tools = false`: trim the description, \
             share the paragraph with the tool that already says it, or move it to a comment \
             where it costs nothing. If the words are genuinely worth paying for, raise the \
             number -- deliberately."
        );
    }

    /// A description is a JSON string, so a wrapped source line is not a wrapped message.
    ///
    /// Rust only continues a string literal across lines with a single `\` at the end; `\\` is an
    /// escaped backslash, and the newline and the indentation that follow it land in the text the
    /// model reads. That is what `bash`'s `download` description did -- one stray backslash and
    /// fifty characters of whitespace in the middle of a sentence -- and nothing noticed, because
    /// it looks right in the source.
    #[test]
    fn no_description_carries_source_formatting() {
        let dir = TempDir::new("schema-text");
        let box_ = toolbox(dir.path(), false);
        for (name, description, schema) in box_.all_specs() {
            let mut strings = vec![description];
            collect_strings(&schema, &mut strings);
            for text in strings {
                assert!(
                    !text.contains('\n'),
                    "{name} has a newline in a description the model reads: {text:?}"
                );
                assert!(
                    !text.contains("  "),
                    "{name} has a run of spaces in a description the model reads: {text:?}"
                );
            }
        }
    }

    /// A paragraph that belongs to two tools is written once.
    ///
    /// `timeout_secs` and `background` were written separately in `bash` and in `exec` and had
    /// already drifted into different words for the same rule, which costs that paragraph twice on
    /// every request to say one thing.
    #[test]
    fn a_parameter_shared_by_two_tools_is_described_once() {
        let dir = TempDir::new("schema-shared");
        let box_ = toolbox(dir.path(), false);
        let specs = box_.all_specs();
        for param in ["timeout_secs", "background"] {
            let mut seen: Vec<(String, String)> = Vec::new();
            for (name, _, schema) in &specs {
                if let Some(p) = schema
                    .get("properties")
                    .and_then(|p| p.get(param))
                    .and_then(|p| p.get("description"))
                    .and_then(|d| d.as_str())
                {
                    seen.push((name.clone(), p.to_string()));
                }
            }
            // `task` and `tasks` wait by default and say so; `job_op`'s timeout means something
            // else entirely. What must not differ is the pair that means the same thing.
            let waits: Vec<&(String, String)> = seen
                .iter()
                .filter(|(n, _)| n == "bash" || n == "exec")
                .collect();
            assert_eq!(waits.len(), 2, "{param} is missing from bash or exec: {seen:?}");
            assert_eq!(
                waits[0].1, waits[1].1,
                "bash and exec describe {param} differently, so one of them is paid for twice: \
                 {:?} against {:?}",
                waits[0].1, waits[1].1
            );
        }
    }

    fn collect_strings(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(t) => out.push(t.clone()),
            Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
            Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
            _ => {}
        }
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

    /// A patch cannot name a CRLF ending, so a CRLF file needs the sentence rather than a guess.
    ///
    /// The parser reads the patch with `str::lines`, which drops the `\r` before each `\n`, and the
    /// file's own lines keep theirs -- so no hunk taken from this file can match, however carefully
    /// it was copied. "The text to replace is not in the file" is true and useless; the sentence has
    /// to say which tool does work here (`edit`, whose `old_string` is raw text and can carry the
    /// `\r`) and must carry those escapes as *text*, because a real carriage return lands inside a
    /// message that goes to the answer strip. Both are asserted, and the file is asserted unchanged,
    /// because a refusal that half-applied would be worse than the confusion it replaces.
    ///
    /// Not `cfg(windows)`: the endings are in the file and the comparison is string code, so this is
    /// about a file's bytes rather than about the platform the test runs on.
    #[tokio::test]
    async fn a_patch_against_a_crlf_file_says_so_and_names_the_tool_that_works() {
        let dir = TempDir::new("patch-crlf");
        std::fs::write(dir.path().join("dos.txt"), "one\r\ntwo\r\n").unwrap();
        let tools = tools(dir.path());
        tools
            .invoke("read", &json!({ "path": "dos.txt" }))
            .await
            .expect("read first");

        let text = "*** Begin Patch\n\
                    *** Update File: dos.txt\n\
                    -two\n\
                    +three\n\
                    *** End Patch\n";
        let error = tools
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("a patch cannot match a CRLF file");
        let message = error.to_string();
        assert!(
            message.contains("CRLF"),
            "the refusal must name the line endings: {message}"
        );
        assert!(
            message.contains("`edit`"),
            "and the tool that does work here: {message}"
        );
        assert!(
            message.contains("\\r\\n"),
            "the escapes must be text a model can type, not bytes: {message:?}"
        );
        assert!(
            !message.contains('\r'),
            "a carriage return in a message moves the cursor to column 0: {message:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dos.txt")).unwrap(),
            "one\r\ntwo\r\n",
            "nothing may be written when a hunk does not match"
        );

        // An LF file is not blamed for line endings it does not have.
        std::fs::write(dir.path().join("unix.txt"), "one\ntwo\n").unwrap();
        tools
            .invoke("read", &json!({ "path": "unix.txt" }))
            .await
            .expect("read first");
        let text = "*** Begin Patch\n\
                    *** Update File: unix.txt\n\
                    -nope\n\
                    +x\n\
                    *** End Patch\n";
        let error = tools
            .invoke("apply_patch", &json!({ "patch": text }))
            .await
            .expect_err("no match is still an error");
        assert!(
            !error.to_string().contains("CRLF"),
            "an LF file must not be blamed for line endings: {error}"
        );
    }
}
