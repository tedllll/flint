//! The agent loop.
//!
//! One iteration is: ask the provider -> collect streamed events -> if the
//! model requested tools, run them and feed the results back -> repeat until it
//! answers without asking for a tool, or the step cap is reached.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::Config;
use crate::context;
use crate::event::{Event, Message, ToolCall, Usage};
use crate::provider::Provider;
use crate::session::{SessionEvent, SessionWriter};
use crate::tools::{self, ToolBox};

pub const SYSTEM_PROMPT: &str = "\
You are flint, a command-line agent that works directly on the user's machine.

The task is whatever the user asks for. It may be building something, exploring \
or learning a codebase, running and debugging, setting up a machine, changing \
configuration, or diagnosing something that is broken. Do not assume it is a \
repair, and do not go looking for damage that was not reported.

Rules:
- Act, do not narrate. Use the tools to inspect the real system before making \
  claims about it.
- Read a file before editing it. Never guess at its contents.
- Prefer small, verifiable steps over large changes.
- When a command fails, read the error and adapt. Do not repeat the same \
  failing command.
- Report exactly what you changed and why.
- Be concise. The user is looking at a terminal.
- Prefer the `read`, `write`, `edit`, `list`, `glob` and `grep` tools over shell \
  equivalents. `glob` finds files by name and `grep` searches file contents, both \
  recursively; they behave identically on every platform, which the shell does not. \
  Reach for them instead of `find`, `dir /s`, `findstr` or `grep -r`.
- Prefer `fetch` over `curl` for reading a web page. It strips the markup, bounds how much
  comes back, and says where the page came from; `curl` hands you the raw bytes, which for a
  page is mostly markup and is what gets cut off when it is long. Use `curl` when you want
  the raw bytes on purpose -- an API, a download, a file.
- Prefer `exec` over `bash` for running a program. `exec` takes the program and one \
  array of arguments, so nothing between you and the program re-reads what you wrote: \
  quotes, spaces, backslashes and non-ASCII text inside an argument arrive intact. Use \
  `bash` when you actually need shell syntax -- pipes, redirects, `&&`, variables, \
  globbing -- and remember that a command string is parsed by the shell before the \
  program sees it.
- A prompt may contain `<file path=\"...\">` blocks. Those are files the caller attached, \
  in full, in place of naming them -- not a tool result of yours and not a path to go and \
  look up. What is between the tags is the whole file, so do not spend a `read` confirming \
  it, and do not assume the file is longer than what is there.

About flint itself:
- flint is the program you are running inside. It is normally an installed \
  binary, not a source checkout, so editing files in the working directory does \
  not change how flint behaves.
- The model, providers, API keys, shell, step limit and proxy live in a \
  hand-editable TOML config file, not in code. Its path is in the facts below. \
  `/config` shows it, and `/reload` re-reads it after a hand edit.
- Prefer `/model`, `/provider` and `/provider key` for those settings: they edit \
  the config for the user and take effect at once.
- So when the user asks you to change flint's own configuration -- the model, the \
  provider, a key -- change the configuration, or tell them the command to run. Do \
  not go hunting through flint's source to change how flint behaves.
- Sessions are append-only JSONL, one file per session, in the directory below.";

/// Where commands run, generated per platform.
///
/// The shell dialect is settled at build time and baked into the binary, so it
/// costs nothing at run time. The alternative -- describing every platform and
/// letting the model work out which applies -- spends tokens on every request
/// and invites the model to guess wrong.
mod platform {
    #[cfg(windows)]
    pub const SHELL_HINT: &str =
        "It is cmd.exe: use `dir`, `type`, `echo %CD%`. `ls`, `pwd`, `cat`, `find` and `grep` are not available.";

    #[cfg(not(windows))]
    pub const SHELL_HINT: &str =
        "It is a POSIX shell: `ls`, `cat`, `find` and `grep` are available.";

    #[cfg(windows)]
    pub const PATH_SEPARATOR: char = '\\';

    #[cfg(not(windows))]
    pub const PATH_SEPARATOR: char = '/';
}

/// Build the system prompt with the facts the model cannot guess.
///
/// Deliberately short. The binary already knows its own OS and architecture, so
/// stating them spends tokens on something the model cannot act on differently.
/// What it *cannot* infer is the shell dialect -- trying `ls` in cmd.exe is the
/// single most common way this agent wastes a turn -- and where commands will
/// run, because the working directory is set at run time.
///
/// Discovers the workspace itself, for the callers that have only a config and a directory.
/// The constructors below do the walk once and call `prompt_with_workspace` with what they found,
/// because they need the same answer for two things -- the prompt, and the skill names the page's
/// menu is built from.
pub fn build_system_prompt(config: &Config, cwd: &std::path::Path) -> String {
    prompt_with_workspace(
        config,
        cwd,
        &context::Workspace::discover(cwd, &config.skill_dirs),
    )
}

fn prompt_with_workspace(
    config: &Config,
    cwd: &std::path::Path,
    workspace: &context::Workspace,
) -> String {
    let shell = tools::probe_shell(&config.shell, &config.shell_args).join(" ");

    let mut prompt = format!(
        "{SYSTEM_PROMPT}\n\n\
         Local facts:\n\
         - Shell: `{shell}`. {hint}\n\
         - Working directory: {cwd} (path separator `{sep}`).\n\
         - flint config: {config}\n\
         - flint sessions: {sessions}",
        hint = platform::SHELL_HINT,
        cwd = cwd.display(),
        sep = platform::PATH_SEPARATOR,
        config = crate::config::config_path().display(),
        sessions = crate::config::sessions_dir().display(),
    );

    // Which PowerShell this machine has, asked once per process. A model on Windows that needs
    // PowerShell otherwise has to guess whether it is 5.1 or 7, and the two do not parse the
    // same language: a script written with `??` is a parse error with no hint as to why.
    #[cfg(windows)]
    if let Some(facts) = tools::powershell_facts() {
        prompt.push_str(&format!("\n- PowerShell (use the `pwsh` tool): {facts}"));
    }

    // What the project has written down. Discovered here, once per agent, because the
    // answer depends on the working directory -- and rebuilt by `/model` and friends,
    // which is how a file added mid-session gets noticed without a restart. The same
    // walk gives the skill names the page's menu offers, so the two cannot disagree
    // about what this run has.
    let mode =
        context::Instructions::parse(&config.instructions).unwrap_or(context::Instructions::Hint);
    let note = workspace.prompt_note(mode);
    if !note.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&note);
    }
    prompt
}

pub struct Agent {
    provider: Provider,
    tools: ToolBox,
    history: Vec<Message>,
    max_steps: usize,
    /// How much conversation one request may carry, in characters. See `trim_old_turns`.
    ///
    /// Captured from the config when the agent is built, like `max_steps`: `/reload` rebuilds the
    /// agent, which is what makes a changed value take effect.
    max_request_chars: usize,
    readonly: bool,
    cwd: PathBuf,
    writer: Option<SessionWriter>,
    last_usage: Option<Usage>,
    /// Whether the last turn ended because it ran out of steps rather than because the model
    /// stopped asking.
    ///
    /// Kept here because this loop is the only thing that knows: when the `for` runs out, the
    /// difference between "the model had nothing more to say" and "we stopped listening" is a fact
    /// about *this* code and nothing else. The warning the loop already emits is for a person
    /// reading a terminal; this is for a caller that has to decide whether the answer it holds is
    /// finished, which is not a judgement a string can carry.
    ran_out_of_steps: bool,
    /// How often each tool call has been made this turn, by name and arguments.
    ///
    /// Per turn rather than per session: an identical call in a later turn follows a new
    /// question, and a file the last turn read may legitimately have changed since.
    repeats: std::collections::HashMap<String, usize>,
    /// The answer drawn in the step now in flight, kept where dropping the turn cannot
    /// reach it.
    ///
    /// A turn is interrupted by *dropping its future*, so every local of that future goes
    /// with it -- including the step's own accumulator, which is why this is a field. What
    /// the user has already read must not go with it: reported from a real session, half an
    /// article was on screen, `/stop` was typed, and "finish writing it" was answered by a
    /// model with no record of a word of it. Deltas land here as they are drawn;
    /// [`Agent::commit_drawn_answer`] turns whatever is left into a message when a turn ends
    /// without finishing its step.
    drawn: String,
    /// The names of the skills this run was given, from the same walk as its system prompt.
    ///
    /// Kept so the page's menu can offer `/skills <name>` without walking the skill directories
    /// again on every state frame -- the frame is built after every line, and a directory walk per
    /// line is not the comparison the frame's own comment claims it is. It also makes the menu mean
    /// the right thing: these are the skills the *model* was told about, and `/reload` (which
    /// rebuilds this agent) is what makes a new one appear. `/skills` typed by hand still discovers
    /// fresh, because that is a person asking what is on disk right now.
    skills: Vec<String>,
    /// The names of the saved prompts this run found, for the page's menu.
    ///
    /// Not the model's: nothing about a prompt file enters the system prompt or a tool schema, which
    /// is what keeps a directory of long templates free until one is typed. It is still discovered
    /// once per agent for the page's sake -- see [`Agent::prompts`].
    prompts: Vec<String>,
    /// The system prompt without the answer shape, so the shape can be added or dropped later.
    ///
    /// See [`Agent::hold_to_schema`]: a resumed session's schema is read out of its file, which
    /// happens after the agent may already exist, and appending to a prompt whose base nobody kept
    /// would append to the previous append.
    base_prompt: String,
    /// The answer shape this conversation is held to, if the caller asked for one.
    schema: Option<crate::schema::Schema>,
    /// Whether a peer's message is relayed to the model, which is off unless somebody asked for it.
    ///
    /// Off is the whole safety argument of the mailbox: anything that can write a mailbox file could
    /// otherwise steer this tool loop, and this run has no permission layer to catch it. It is a field
    /// rather than a parameter of [`Agent::note_peer`] because it can be changed *while a run is open*
    /// (`/hear-peers on`), and the decision in force when a message arrives is the one that applies to
    /// that message.
    hears_peers: bool,
    /// Peer messages this run is going to relay, waiting for the next request to be built.
    ///
    /// Between turns rather than inside one: the mailbox is read when nothing is in flight, so a
    /// message cannot arrive in the middle of a tool loop and change what a request already being
    /// written says. Drained by [`Agent::with_peers`] into the request *view* -- never into `history`,
    /// which is what makes the relay a decision and not a permanent change to the conversation.
    peer_inbox: Vec<(String, String)>,
    /// `--no-session`: this run writes no conversation, whichever door would open one.
    ///
    /// Not derivable from `writer`, which is `None` for one more reason -- a conversation that has
    /// said nothing yet has no file -- and `writer` changes while a run is open, because `/new` and
    /// `/resume` build one. This is the flag's promise rather than the state of a file, and it is a
    /// field on the run for the same reason the writer is: `/model`, `/provider`, `/reload` and the
    /// page's switch rows all rebuild the agent through `continue_conversation`, which *creates* a
    /// session when the old one has no file. Without this, one `/reload` would leave exactly the file
    /// the flag promised not to leave.
    no_session: bool,
}

impl Agent {
    /// The system prompt for this run, including run-time facts.
    ///
    /// Exposed so that resuming a session rebuilds it rather than reusing a
    /// stored copy: the shell dialect and working directory are properties of
    /// *now*, not of whenever the transcript happened to be written.
    pub fn build_system_prompt(config: &Config, cwd: &std::path::Path) -> String {
        build_system_prompt(config, cwd)
    }

    pub fn new(
        config: &Config,
        provider: Provider,
        readonly: bool,
        cwd: PathBuf,
        writer: Option<SessionWriter>,
    ) -> Self {
        // Spill files belong to one conversation, so that a person reading them later
        // knows which run produced them. A run with no conversation of its own still needs a
        // directory that is its own: `unattached` alone is one directory for every session-less run
        // on the machine and the files in it are numbered from 1, so two `--no-session` runs at once
        // would write over each other's `1.txt`, and the second one's bytes are what the first one's
        // request would be told to read. See `tools::unattached_spill_dir`.
        let session_tag = writer
            .as_ref()
            .and_then(|w| w.path().file_stem())
            .map(|s| s.to_string_lossy().to_string());
        let spill_dir = match &session_tag {
            Some(tag) => crate::config::spill_dir().join(tag),
            None => crate::tools::unattached_spill_dir(),
        };
        // The same id is what a child is told started it, so the child's own session can say where it
        // came from and stay out of the person's list. A run with no conversation is not a session and
        // its name is not passed on: with nothing of its own, it has nothing true to be the parent of.
        let parent = session_tag.clone();
        let tools = ToolBox::new(config, readonly, cwd.clone())
            .with_spill_dir(spill_dir)
            .with_task_endpoint(provider.name(), provider.model())
            .with_task_parent(parent)
            // And the flag itself, one step out: a child is a conversation this run asked for, so a
            // run that keeps none has to say so to its children or `--no-session` would leave a file
            // behind through the one door it opened itself.
            .with_task_no_session(session_tag.is_none())
            // And what a *command* is told about the run it is in, as opposed to what a child run
            // is told (`FLINT_PARENT`, above): a `bash` script that wants to find the transcript,
            // or to ask the same endpoint a second question, has no other way to know.
            .with_run_env(
                writer.as_ref().map(|w| w.path().to_path_buf()),
                provider.name(),
                provider.model(),
            );
        // One walk, three answers: the prompt's note, the page's skill menu and the page's prompt
        // menu. See `Agent::skills`.
        let workspace = context::Workspace::discover(&cwd, &config.skill_dirs);
        let skills = workspace.skill_names();
        let prompts = workspace.prompt_names();
        let base_prompt = prompt_with_workspace(config, &cwd, &workspace);
        Agent {
            provider,
            tools,
            history: vec![Message::system(base_prompt.clone())],
            max_steps: config.max_steps,
            max_request_chars: config.max_request_chars,
            readonly,
            cwd,
            writer,
            last_usage: None,
            ran_out_of_steps: false,
            repeats: std::collections::HashMap::new(),
            drawn: String::new(),
            skills,
            prompts,
            base_prompt,
            // No shape until a caller says so: an agent built for the REPL answers in prose.
            schema: None,
            // Off until somebody says otherwise: `--hear-peers`, or `/hear-peers on` at the prompt.
            hears_peers: false,
            peer_inbox: Vec::new(),
            // Off until the command line says otherwise: `Agent::new` is also how the tests, the
            // examples and every rebuilt agent in a run are made, and a default of "write nothing"
            // would be a surprise in all of them.
            no_session: false,
        }
    }

    /// Whether this run was told to write no conversation (`--no-session`).
    pub fn no_session(&self) -> bool {
        self.no_session
    }

    /// Say that it was, which the startup path does once and `continue_conversation` carries on.
    pub fn set_no_session(&mut self, yes: bool) {
        self.no_session = yes;
    }

    /// Whether a peer's message is relayed to the model. Off unless a person asked.
    pub fn hears_peers(&self) -> bool {
        self.hears_peers
    }

    /// Turn the relay on or off.
    ///
    /// Turning it *off* does not un-say what a model has already been told, so it does not pretend to:
    /// what it does is stop the next message from being relayed and drop whatever was waiting, because
    /// a person who has just turned this off does not want the queue arriving with the next question.
    /// Turning it on affects messages from here on; the mailbox is read between turns, so this is
    /// always about the future and never about a request in flight.
    pub fn hear_peers(&mut self, on: bool) {
        self.hears_peers = on;
        if !on {
            self.peer_inbox.clear();
        }
    }

    /// The system prompt as it should read now: the run's facts, plus the answer shape if this
    /// conversation is held to one.
    fn system_prompt(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{}\n\n{}", self.base_prompt, schema.prompt_section()),
            None => self.base_prompt.clone(),
        }
    }

    /// Hold this conversation's answers to a shape, or stop holding them to one.
    ///
    /// A setter rather than a sixth argument to [`Agent::new`] because the shape is not always known
    /// when an agent is built, and the fact is not the caller's to keep: `--resume` reads the schema
    /// out of the session *file*, and the three places that rebuild an agent (`/provider`, `/model`,
    /// `/reload`) carry the conversation across -- so a shape living in a local would be dropped by
    /// the first switch, silently, in a run that had promised a caller JSON. The prompt is rebuilt
    /// here and by [`Agent::splice_loaded_history`], and nowhere else, so there is one answer to
    /// "what does the model see".
    pub fn hold_to_schema(&mut self, schema: Option<crate::schema::Schema>) {
        // The request side of the same promise, and set here rather than by the caller so the two
        // cannot come apart: a prompt that describes a shape, sent to a server allowed to answer in
        // prose, is a promise flint would then have to make good on by itself.
        self.provider.expect_json(schema.is_some());
        self.schema = schema;
        // Built before the borrow: the prompt reads the field just written.
        let prompt = self.system_prompt();
        if let Some(system) = self.history.first_mut() {
            *system = Message::system(prompt);
        }
    }

    /// Write the answer shape into this conversation's file.
    ///
    /// Called by the run that was *told* a shape -- `--schema` or `--no-schema` -- and not by a run
    /// that merely inherited one, so a file gains a line when someone decides something and stays
    /// quiet when nobody did. `None` records that the shape was dropped.
    pub fn record_schema(&mut self, schema: Option<&serde_json::Value>) -> Result<()> {
        match &mut self.writer {
            Some(writer) => writer.schema(schema),
            // No file to write to (a run with no session, a `--fork` before seeding): the schema is
            // still in force for this run, it is just not being kept anywhere.
            None => Ok(()),
        }
    }

    /// The shape this conversation's answers are held to, if any.
    pub fn schema(&self) -> Option<&crate::schema::Schema> {
        self.schema.as_ref()
    }

    /// Hold this conversation at a reasoning level.
    ///
    /// A setter, for [`Agent::hold_to_schema`]'s reason: a resumed conversation's level is read out of
    /// its *file*, which happens after the agent may already exist, and the places that rebuild an
    /// agent (`/provider`, `/model`, `/reload`) carry the conversation across -- so a level living in
    /// a local would be quietly dropped by the first switch, and the person's `/thinking high` would
    /// last until they changed model. The *field* the level goes in is the provider's and comes with
    /// the provider, so a switch re-reads it rather than carrying the old endpoint's name along.
    pub fn hold_to_thinking(&mut self, level: &str) {
        self.provider.set_thinking(level);
    }

    /// Write the reasoning level into this conversation's file.
    ///
    /// Called by the run that was *told* a level -- `--thinking`, or `/thinking` typed by hand -- and
    /// not by a run that merely inherited one, which is [`Agent::record_schema`]'s rule and for the
    /// same reason: a file gains a line when somebody decided something.
    pub fn record_thinking(&mut self, level: &str) -> Result<()> {
        match &mut self.writer {
            Some(writer) => writer.thinking(level),
            // No file to write to (a run with no session): the level is in force for this run and is
            // just not being kept anywhere.
            None => Ok(()),
        }
    }

    /// The reasoning level in force, and the field it would go in (empty when this provider is sent
    /// no reasoning parameter at all).
    pub fn thinking(&self) -> &str {
        self.provider.thinking()
    }

    /// The field this run's requests carry a reasoning level in, empty when they carry none.
    pub fn thinking_field(&self) -> &str {
        self.provider.thinking_field()
    }

    /// Where a compaction would cut this conversation: the newest question, when there is something
    /// above it to fold.
    ///
    /// A **question** rather than a message count, which is the rule the request-side trimming already
    /// follows and the one Pi's compaction states outright: a cut inside a tool exchange would leave
    /// the model holding half a step -- a question's answer with the question gone, or a tool result
    /// whose call was summarized away. `None` when the newest question is the first thing in the
    /// conversation, because a summary of nothing is not worth a request.
    pub fn compaction_cut(&self) -> Option<usize> {
        let cut = self
            .history
            .iter()
            .rposition(|m| matches!(m, Message::User { .. }))?;
        (cut >= 2).then_some(cut)
    }

    /// What a fold would cost and where it would land, or `None` when there is nothing to fold.
    ///
    /// Both halves at once on purpose: the index the messages are cut at and the byte offset in the
    /// file those messages start at are two readings of one decision, and a caller that computed them
    /// apart could write a pointer that folds a different question than the one it summarized.
    pub fn compaction_plan(&self) -> Result<Option<(usize, u64)>> {
        let Some(cut) = self.compaction_cut() else {
            return Ok(None);
        };
        let Some(path) = self.session_path().filter(|p| p.exists()) else {
            return Ok(None);
        };
        Ok(crate::session::last_question_offset(&path)?.map(|from| (cut, from)))
    }

    /// Ask the model to summarize everything before `cut`.
    ///
    /// A request of its own rather than a turn: it carries **no tools**, so nothing the model says
    /// here can act, and the answer is a paragraph rather than a step in a loop. This is the only
    /// thing a slash command asks a model for on the conversation's own behalf, which is why
    /// `/compact` says out loud that it is spending a request.
    ///
    /// The messages asked about are the ones that are about to be folded -- not the whole
    /// conversation, which would make the request as large as the thing it is trying to shrink.
    pub async fn summarize(&self, cut: usize) -> Result<String> {
        let mut asked: Vec<Message> = self.history[..cut.min(self.history.len())].to_vec();
        asked.push(Message::user(COMPACT_INSTRUCTION));
        let mut text = String::new();
        {
            let sink = |event: Event| {
                if let Event::Text(t) = event {
                    text.push_str(&t);
                }
            };
            self.provider.stream_chat(&asked, &[], sink).await?;
        }
        let summary = text.trim().to_string();
        if summary.is_empty() {
            anyhow::bail!("the model returned no summary, so nothing was folded");
        }
        Ok(summary)
    }

    /// Write the fold into the conversation's file.
    ///
    /// Writer-or-ok, like [`Agent::record_thinking`]: a run that keeps no file still compacts what it
    /// is holding, and refusing here would fail the command for a reason the person did not ask about.
    /// What such a run cannot do is come back compacted, which is the honest limit.
    pub fn record_compaction(&mut self, summary: &str, from: u64) -> Result<()> {
        match &mut self.writer {
            Some(writer) => writer.compacted(summary, from),
            None => Ok(()),
        }
    }

    /// Fold everything before `cut` into `summary`, in the conversation this run is holding.
    ///
    /// The file was written first; this is the same fold applied to the messages in memory, so the
    /// *next* request is the smaller one without a reload. The head of the history -- the system
    /// prompt -- stays where it is: a compaction folds what was *said*, not what the run was told it is.
    pub fn apply_compaction(&mut self, summary: &str, cut: usize) {
        if cut == 0 || cut > self.history.len() {
            return;
        }
        let mut next = Vec::with_capacity(self.history.len() - cut + 2);
        next.extend(self.history[..1].iter().cloned());
        next.push(crate::session::compacted_message(summary));
        next.extend(self.history[cut..].iter().cloned());
        self.history = next;
        // The counts of the last turn were about a prompt that no longer exists -- the same reason
        // `set_last_usage` exists for a replacement agent.
        self.last_usage = None;
    }

    /// The answer shape this conversation is being held to, as it would be written to a session.
    ///
    /// The raw schema, not the parsed one: what the file records has to be what the caller wrote, so
    /// that a reader can see the contract without flint's reading of it in the way.
    pub fn schema_json(&self) -> Option<serde_json::Value> {
        self.schema.as_ref().map(|s| s.raw().clone())
    }

    /// The conversation as it stands.
    ///
    /// For the code that has to carry it into a *new* agent: `/model`, `/provider` and
    /// `/reload` all replace the agent wholesale, and a replacement starts empty.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn history_mut(&mut self) -> &mut Vec<Message> {
        &mut self.history
    }

    pub fn last_usage(&self) -> Option<Usage> {
        self.last_usage
    }

    /// Carry the last reported counts into a *replacement* agent.
    ///
    /// Two paths replace the agent while keeping the conversation -- a resume that loaded the file,
    /// and every mid-run rebuild (`/model`, `/provider`, `/reload`, the page's own switch rows) -- and
    /// both hand over the messages and nothing else, so the counts of the last turn used to be lost
    /// by the act of switching. `None` is written rather than skipped: the caller is saying what the
    /// conversation's last usage was, and a replacement that was handed nothing has nothing.
    pub fn set_last_usage(&mut self, usage: Option<Usage>) {
        self.last_usage = usage;
    }

    /// Whether the last turn ended at the step limit, which makes its answer an unfinished one.
    pub fn ran_out_of_steps(&self) -> bool {
        self.ran_out_of_steps
    }

    pub fn readonly(&self) -> bool {
        self.readonly
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }

    /// What a command started from this run is told about it, for the one caller outside the tool
    /// set: the REPL's `!` escape, which runs a command through the same runner `bash` uses and
    /// must not describe a different run to it.
    pub fn run_env(&self) -> &crate::tools::RunEnv {
        self.tools.run_env()
    }

    /// The request body that the next round-trip would send, without sending it.
    ///
    /// Built from the same two things `step` builds -- the *pruned* history and the tool
    /// specs -- through the same `provider::request_body` the client uses, because the
    /// question this answers is not "what is in the session file". It is "what does the
    /// model actually read", and those are different on purpose: request-side pruning drops
    /// stale tool output that the session file keeps forever. A preview assembled any other
    /// way would answer a different question, confidently.
    ///
    /// `user_input` is the message that would be sent next, so the command can show the
    /// request for something the user is *about* to say, not only for what has been said.
    pub fn request_preview(&self, user_input: Option<&str>) -> serde_json::Value {
        let mut history = self.history.clone();
        if let Some(text) = user_input {
            history.push(Message::user(text));
        }
        let view = prune_tool_output(&history);
        // The two bounds in the order that keeps the request smallest: pruning shrinks the stale
        // results *inside* the turns first, and only what is still over the budget after that costs
        // whole turns -- which is the more expensive thing to lose, because a dropped turn takes its
        // question with it.
        let mut view = trim_old_turns(&view, self.max_request_chars);
        // The two things a real request carries that the session file does not: what a peer said, and
        // what flint has to report about jobs that ended. Both are *shown* here without being consumed
        // -- a preview that delivered them would change the next real request, and delivering a peer's
        // words twice is one of the things the relay promises not to do.
        if let Some(relay) = peer_relay(&self.peer_inbox) {
            view.push(relay);
        }
        if let Some(report) = job_report(&crate::tools::settled_unreported(false)) {
            view.push(report);
        }
        crate::provider::request_body(
            self.provider.model(),
            &view,
            &self.tools.specs(),
            // The preview has to carry it: `flint debug prompt-input` on a schema run is how you see
            // that the shape went into the prompt *and* into `response_format`, and a preview that
            // quietly left one out would be a preview of a different request.
            self.schema.is_some(),
            // ...and the reasoning level, for the same reason one step out: whether a request asks
            // for reasoning is not visible anywhere in the conversation, so a preview that dropped it
            // would be the only way to see it -- and would show the wrong thing.
            self.provider.thinking_spec(),
        )
    }

    /// Replace this conversation's history with a loaded one, behind a fresh system prompt.
    ///
    /// The stored prompt carries run-time facts -- the shell dialect, the working directory,
    /// the instruction files -- that a transcript from another machine, or from another
    /// directory, cannot be trusted to still be right about. So it is rebuilt and the stored
    /// one is dropped rather than kept.
    pub fn splice_loaded_history(&mut self, cfg: &Config, cwd: &std::path::Path, loaded: Vec<Message>) {
        let workspace = context::Workspace::discover(cwd, &cfg.skill_dirs);
        // The base is kept as well as used: `hold_to_schema` rebuilds this same prompt, and a shape
        // adopted from the loaded session has to be able to join it without the two appends stacking.
        self.base_prompt = prompt_with_workspace(cfg, cwd, &workspace);
        let mut merged = vec![Message::system(self.system_prompt())];
        merged.extend(
            loaded
                .into_iter()
                .filter(|m| !matches!(m, Message::System { .. })),
        );
        self.history = merged;
        // The prompt was rebuilt, so the menu the page draws is rebuilt with it: a page offering a
        // skill this run's prompt no longer mentions would be promising more than the model has.
        self.skills = workspace.skill_names();
        self.prompts = workspace.prompt_names();
    }

    /// The skills this run was given, in the order the model sees them.
    pub fn skills(&self) -> &[String] {
        &self.skills
    }

    /// The saved prompts this run found, for the page's menu.
    ///
    /// Unlike `skills` these are *not* what the model was given -- a template never reaches the
    /// prompt -- so the page is the only reader, and the list is here for the reason the skill list
    /// is: the frame is built after every line, and a directory walk per line is not free.
    pub fn prompts(&self) -> &[String] {
        &self.prompts
    }

    /// The file this conversation is being appended to, when it is being saved at all.
    ///
    /// The commands that manage sessions need it to know which conversation they are
    /// looking at: `/name` writes to it, and `/archive` and `/delete` have to be able to
    /// refuse the one that is open.
    pub fn session_path(&self) -> Option<PathBuf> {
        self.writer.as_ref().map(|w| w.path().to_path_buf())
    }

    /// Name this conversation, by appending a `title` event.
    pub fn name_session(&mut self, name: &str) -> Result<()> {
        match &mut self.writer {
            Some(writer) => writer.title(name),
            None => anyhow::bail!("this conversation is not being saved"),
        }
    }

    /// Record that a peer said something: in the session file always, and in the next request only
    /// when this run was asked to hear peers.
    ///
    /// Not in `history` either way is the point of the default. A peer's words are shown to the person
    /// at the terminal and written here so the conversation reads back whole, and by default they are
    /// **not** put anywhere a request is built from. Anything that can write a mailbox could otherwise
    /// steer this tool loop, and this run has no permission layer to catch it: `docs/agents.md` calls
    /// that the widest hole in the system, and this is the method that does not open it. `--hear-peers`
    /// opens it deliberately, which is why the relay goes to [`Agent::with_peers`] (the request view)
    /// rather than here: a message heard once must not become part of the conversation that a resumed
    /// run is rebuilt from.
    ///
    /// Returns whether it was written, because a run with no session file (one-shot with no prompt,
    /// or a test) should not pretend it kept a record.
    pub fn note_peer(&mut self, from: &str, text: &str) -> bool {
        let heard = self.hears_peers;
        if heard {
            self.peer_inbox.push((from.to_string(), text.to_string()));
        }
        let Some(writer) = &mut self.writer else {
            return false;
        };
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        writer
            .append(&SessionEvent::Peer {
                from: from.to_string(),
                text: text.to_string(),
                at,
                heard,
            })
            .is_ok()
    }

    /// The request view of the history, with anything a peer said since the last request relayed.
    ///
    /// One user-role message for all of them, labelled so the model knows where it came from and that
    /// the person let it through -- a relay that looked like the person's own words would be a worse
    /// lie than the silence it replaces. Deliberately not `history`: this is a view, like
    /// [`prune_tool_output`], so the session file keeps the peer event and nothing else, and a run
    /// resumed from that file starts with no peer words in it.
    fn with_peers(&mut self, mut view: Vec<Message>) -> Vec<Message> {
        if self.peer_inbox.is_empty() {
            return view;
        }
        let said = std::mem::take(&mut self.peer_inbox);
        if let Some(relay) = peer_relay(&said) {
            view.push(relay);
        }
        view
    }

    /// The request view with any job that ended since the last request reported.
    ///
    /// The default `task` is to start a child and not wait for it, which would be a way to lose an
    /// answer if nothing ever said the child had ended: a model that has moved on has no reason to
    /// poll, and the answer costs what it costs whether or not anybody reads it. So flint says so
    /// itself, once, in the request that follows -- the same treatment a peer's words get, and a view
    /// for the same reason: the session file is the record of what *happened*, and a conversation
    /// resumed from it is not told about a job that ended days ago. Marking it here is what makes it
    /// once: the next request finds it reported and says nothing.
    fn with_jobs(&mut self, mut view: Vec<Message>) -> Vec<Message> {
        let lines = crate::tools::settled_unreported(true);
        if let Some(report) = job_report(&lines) {
            view.push(report);
        }
        view
    }

    /// Run one user turn to completion, reporting progress through `sink`.
    pub async fn run(&mut self, user_input: &str, mut sink: impl FnMut(Event)) -> Result<()> {
        // Before anything can be sent: if the previous turn was interrupted while it
        // had tool calls outstanding, the history ends with those unanswered and every
        // later request is rejected with "An assistant message with 'tool_calls' must
        // be followed by tool messages responding to each 'tool_call_id'". That made
        // the session permanently unusable, not just the one turn.
        self.close_dangling_tool_calls();
        self.repeats.clear();
        // A turn begins here, so the answer it ends with is a fresh question: the flag means "this
        // turn ran out of steps", not "some turn once did".
        self.ran_out_of_steps = false;

        self.history.push(Message::user(user_input));
        self.record(SessionEvent::Chat {
            message: Message::user(user_input),
        });

        for step in 0..self.max_steps {
            let outcome = self.step(&mut sink).await?;

            if outcome.tool_calls.is_empty() {
                if outcome.text.is_empty() && outcome.reasoning.is_empty() {
                    sink(Event::Warning(
                        "the model returned an empty response".to_string(),
                    ));
                }
                // Persist the final answer. Without this the transcript holds
                // the user's questions and the tool calls, but none of the
                // actual replies -- so a resumed conversation is missing every
                // answer, and the model is asked to continue from a one-sided
                // record of what happened.
                if !outcome.text.is_empty() || !outcome.reasoning.is_empty() {
                    let answer = Message::Assistant {
                        content: non_empty(outcome.text.clone()),
                        reasoning: non_empty(outcome.reasoning.clone()),
                        tool_calls: Vec::new(),
                    };
                    self.history.push(answer.clone());
                    self.record(SessionEvent::Chat { message: answer });
                }
                // Committed, so nothing is left over for an interrupt to keep: what is in
                // the history now is what was drawn.
                self.drawn.clear();
                sink(Event::Done);
                return Ok(());
            }

            // Commit the assistant turn (text + the tool calls it requested).
            let assistant = Message::Assistant {
                content: non_empty(outcome.text.clone()),
                reasoning: non_empty(outcome.reasoning.clone()),
                tool_calls: outcome.tool_calls.clone(),
            };
            self.history.push(assistant.clone());
            self.record(SessionEvent::Chat { message: assistant });
            // The text of this step went into the history with the calls it asked for, so an
            // interrupt from here on has nothing of its own left to keep.
            self.drawn.clear();

            // Every call in this message is started before any of them is waited for. That is the
            // whole of item 12 from the reading of Pi: a model that asks for four files, or four
            // commands, in one message is already waiting for all four, so running them one after
            // another spends three waits nobody asked for. The unit is the **message** and not the
            // step -- what the model asked for together is what runs together -- and the tool set is
            // built for it: `Tools::invoke` takes `&self` and keeps what it must remember behind a
            // `Mutex`, so calls share the tools and not their answers.
            //
            // Arguments are parsed first, because a call nobody can run must not hold up the ones
            // that can. A call whose arguments do not parse is answered with the parse error, which
            // is what actually gets it fixed.
            let mut parsed: Vec<Result<serde_json::Value, String>> =
                Vec::with_capacity(outcome.tool_calls.len());
            let mut runnable: Vec<(usize, serde_json::Value)> = Vec::new();
            for (index, call) in outcome.tool_calls.iter().enumerate() {
                match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                    Ok(args) => {
                        parsed.push(Ok(args.clone()));
                        runnable.push((index, args));
                    }
                    Err(e) => parsed.push(Err(e.to_string())),
                }
            }

            // One future per call, all of them borrowing the tool set and none of them touching the
            // conversation. `join_all` resolves in the order it was given, which is the order the
            // calls arrived in, and the pairing below depends on that.
            //
            // Two calls that write one file cannot lose each other's work by running together: the
            // read-before-mutate gate refuses the second one, because the file changed since *that*
            // call read it. That gate is what makes this safe rather than lucky, and it is the reason
            // no tool-by-tool rule about what may run at once is needed here.
            let futures = runnable
                .iter()
                .map(|(index, args)| self.tools.invoke(&outcome.tool_calls[*index].name, args))
                .collect::<Vec<_>>();
            let done = futures_util::future::join_all(futures).await;
            let mut results: Vec<Option<Result<String, anyhow::Error>>> =
                (0..outcome.tool_calls.len()).map(|_| None).collect();
            for ((index, _), result) in runnable.iter().zip(done) {
                results[*index] = Some(result);
            }

            // Then the reporting, in the order the model asked for, whether the calls ran together or
            // one at a time. Ordered on purpose: the transcript is read top to bottom and the pairing
            // of a call with its result is the only structure in it. What was concurrent is the
            // *waiting*, and no line of the transcript ever carried that.
            for (index, call) in outcome.tool_calls.iter().enumerate() {
                let args = match &parsed[index] {
                    Ok(args) => args,
                    Err(e) => {
                        let msg = format!(
                            "invalid JSON arguments for tool '{}': {e}. Raw: {}",
                            call.name,
                            crate::util::truncate(&call.arguments, 400)
                        );
                        // Both, and in this order: the warning is for the user, the result
                        // is for the record and for the model. Announcing a call without
                        // ever reporting how it ended leaves a transcript -- and a
                        // `--json` stream -- with a call that appears to still be running.
                        sink(Event::Warning(msg.clone()));
                        sink(Event::ToolResult {
                            id: call.id.clone(),
                            output: msg.clone(),
                            ok: false,
                        });
                        self.push_tool_result(call, &msg, false);
                        continue;
                    }
                };

                match results[index].take().expect("every parsed call was run") {
                    Ok(output) => {
                        let ok = !output.contains("[exit code:");
                        let output = match self.note_repeat(&call.name, args) {
                            Some(note) => format!("{output}\n{note}"),
                            None => output,
                        };
                        sink(Event::ToolResult {
                            id: call.id.clone(),
                            output: output.clone(),
                            ok,
                        });
                        self.push_tool_result(call, &output, ok);
                    }
                    Err(e) => {
                        let msg = format!("tool '{}' failed: {e:#}", call.name);
                        sink(Event::ToolResult {
                            id: call.id.clone(),
                            output: msg.clone(),
                            ok: false,
                        });
                        self.push_tool_result(call, &msg, false);
                    }
                }
            }

            if step + 1 == self.max_steps {
                sink(Event::Warning(format!(
                    "reached the {}-step limit for this turn. Send another message to continue.",
                    self.max_steps
                )));
            }
        }

        // Leaving the loop any other way is a `return` above; getting here means the `for` ran out,
        // which is the one ending that answers the question with less than the model had to give.
        self.ran_out_of_steps = true;
        sink(Event::Done);
        Ok(())
    }

    /// Record this call and, at a few counts, say so in the result.
    ///
    /// A repeat is sometimes right -- a file another process is writing, a command whose
    /// answer really has changed -- so nothing is blocked and nothing is refused. What is
    /// wrong is repeating it *silently*, turn after turn, while the token budget goes into
    /// the same output and the same reasoning. The note names the count and what it means
    /// and leaves the decision where it belongs.
    ///
    /// Counted by name and arguments together, so reading a different file is not a
    /// repeat. `serde_json`'s objects keep their keys sorted, so two spellings of the
    /// same arguments compare equal.
    fn note_repeat(&mut self, name: &str, args: &serde_json::Value) -> Option<String> {
        let key = format!("{name}\u{1}{args}");
        let count = {
            let seen = self.repeats.entry(key).or_insert(0);
            *seen += 1;
            *seen
        };
        let note = match count {
            3 => format!(
                "[note: this is the 3rd identical call to {name} with the same arguments. \
                 It will return what it returned the first time.]"
            ),
            5 => format!(
                "[note: this is the 5th identical call to {name} with the same arguments. \
                 If the first one did not answer the question, this one will not either.]"
            ),
            8 => format!(
                "[note: this is the 8th identical call to {name} with the same arguments. \
                 That is a loop: say what is blocking you, or try something else.]"
            ),
            _ => return None,
        };
        Some(note)
    }

    /// Record a tool result in history and persist it.
    fn push_tool_result(&mut self, call: &ToolCall, output: &str, _ok: bool) {
        let msg = Message::Tool {
            tool_call_id: call.id.clone(),
            content: output.to_string(),
        };
        self.history.push(msg.clone());
        self.record(SessionEvent::Chat { message: msg });
    }

    /// Answer every tool call that never got an answer, so the history stays valid.
    ///
    /// Dropping the turn future -- which is what typing to interrupt does -- cancels a
    /// tool loop exactly where it stood: the assistant message asking for three tools
    /// can be followed by one result or none. The wire contract has no room for that,
    /// so the turn that follows would fail before it started, and every turn after it
    /// too. The placeholder says plainly what happened, which is more honest than
    /// deleting the exchange and pretending the model never asked.
    fn close_dangling_tool_calls(&mut self) {
        let mut repaired: Vec<Message> = Vec::with_capacity(self.history.len());
        let mut index = 0;
        while index < self.history.len() {
            let message = self.history[index].clone();
            repaired.push(message.clone());
            let Message::Assistant { tool_calls, .. } = &message else {
                index += 1;
                continue;
            };
            if tool_calls.is_empty() {
                index += 1;
                continue;
            }
            // The results for this assistant turn are the tool messages that follow it.
            let mut answered: Vec<String> = Vec::new();
            let mut next = index + 1;
            while let Some(Message::Tool { tool_call_id, .. }) = self.history.get(next) {
                answered.push(tool_call_id.clone());
                repaired.push(self.history[next].clone());
                next += 1;
            }
            for call in tool_calls {
                if !answered.contains(&call.id) {
                    // What is true of every dropped tool call: the result never arrived. What used to
                    // be written here was stronger than that -- "was requested but never ran" -- and
                    // for the two tools that start a process it was false. Measured on a real
                    // session: the child of a `task` that was interrupted ran on for minutes
                    // afterwards, spending the caller's money, while the parent's own record said it
                    // had never run and the model, reading that, offered to run it again.
                    let mut content = format!(
                        "interrupted by the user: the result of '{}' never came back.",
                        call.name
                    );
                    // The children are processes of their own, so they are still there to be
                    // described -- and the place their answers will land is the one fact that turns
                    // this from a loss into something to go and read.
                    if matches!(call.name.as_str(), "task" | "tasks") {
                        for line in crate::tools::children_running() {
                            content.push(' ');
                            content.push_str(&line);
                            content.push('.');
                        }
                    }
                    let placeholder = Message::Tool {
                        tool_call_id: call.id.clone(),
                        content,
                    };
                    self.record(SessionEvent::Chat {
                        message: placeholder.clone(),
                    });
                    repaired.push(placeholder);
                }
            }
            index = next;
        }
        self.history = repaired;
    }

    fn record(&mut self, event: SessionEvent) {
        if let Some(writer) = &mut self.writer {
            if let Err(e) = writer.append(&event) {
                // Through the notice sink rather than `eprintln!`: this happens *during* a turn, and
                // with the strip active a stray write to stderr lands wherever the cursor is -- inside
                // the answer being drawn. The sink also keeps the sentence out of a `--json` run's
                // stdout, which is the other half of why it exists.
                crate::tools::notice(&format!("warning: cannot persist session event: {e:#}"));
            }
        }
    }

    /// Make the answer that was drawn in an unfinished step part of the conversation.
    ///
    /// Called by whoever dropped the turn, because the loop's own cleanup cannot run: an
    /// interrupt is a dropped future, which is the same shape [`Self::close_dangling_tool_calls`]
    /// exists for one layer down. There the cost of not repairing is a request the provider
    /// rejects; here it is worse, because nothing fails -- the model simply has no record of
    /// something the user has just read, and the next thing said about it is answered as if
    /// it had never been written.
    ///
    /// A no-op on a turn that finished, or one that was interrupted before anything was
    /// drawn: both leave `drawn` empty.
    pub fn commit_drawn_answer(&mut self) {
        let text = std::mem::take(&mut self.drawn);
        if text.trim().is_empty() {
            return;
        }
        // Verbatim, with nothing added to say it was cut off. The words are the model's,
        // and a marker inside them would be flint putting words in its mouth; an answer
        // that stopped mid-sentence says what happened better than a note would.
        let message = Message::Assistant {
            content: Some(text),
            reasoning: None,
            tool_calls: Vec::new(),
        };
        self.history.push(message.clone());
        self.record(SessionEvent::Chat { message });
    }

    /// A single provider round-trip. Returns the accumulated deltas.
    async fn step(&mut self, sink: &mut impl FnMut(Event)) -> Result<StepOutcome> {
        let specs = self.tools.specs();
        let mut outcome = StepOutcome::default();
        let mut pending: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut stream_error: Option<anyhow::Error> = None;
        // Captured inside the closure; committed to `self` after the borrow of
        // `self.history` ends.
        let mut usage_update: Option<Usage> = None;

        // What is sent is not what is stored. A turn that runs to the step limit
        // accumulates one tool result per step, and most of them are listings and file
        // dumps the model has already read and summarised -- asking again with all of
        // them is how a working turn walks into the context ceiling. Dropping the old
        // ones from the *request* costs nothing and keeps the session file whole.
        let sent = prune_tool_output(&self.history);
        // ...and, if that was not enough, the oldest turns. Same order as `request_preview`, because
        // the preview's promise is that it is what this sends.
        let sent = trim_old_turns(&sent, self.max_request_chars);
        // ...and anything a peer said that this run was asked to hear. Here rather than in the history
        // for the same reason: the file keeps the `peer` event, the request carries the words, and a
        // run resumed from that file starts without them.
        let sent = self.with_peers(sent);
        // ...and word of any job this run started that has ended unread, for the same reason again.
        let sent = self.with_jobs(sent);

        // Its own borrow of one field, so the closure can write to it while `self.provider`
        // is borrowed for the call. See `Agent::drawn`.
        let drawn = &mut self.drawn;

        {
            let history = &sent;
            let result = self
                .provider
                .stream_chat(history, &specs, |event| match event {
                    Event::Usage(u) => {
                        usage_update = Some(u);
                        sink(Event::Usage(u));
                    }
                    Event::Text(t) => {
                        outcome.text.push_str(&t);
                        drawn.push_str(&t);
                        sink(Event::Text(t));
                    }
                    Event::Reasoning(t) => {
                        outcome.reasoning.push_str(&t);
                        sink(Event::Reasoning(t));
                    }
                    Event::ToolStart { id, name } => {
                        // Arguments are empty at this point: the provider only
                        // knows the function name so far. They arrive in the
                        // `ToolArgs` event below, after the stream ends.
                        pending.insert(id, (name, String::new()));
                    }
                    Event::ToolArgs { id, args } => {
                        if let Some(slot) = pending.get_mut(&id) {
                            slot.1 = args;
                        } else {
                            // No matching ToolStart: keep the call rather than
                            // silently dropping it.
                            pending.insert(id, (String::new(), args));
                        }
                    }
                    other => sink(other),
                })
                .await;

            if let Err(e) = result {
                stream_error = Some(e);
            }
        }

        if let Some(e) = stream_error {
            return Err(e);
        }

        for (id, (name, arguments)) in pending {
            if name.is_empty() {
                continue;
            }
            // Announce the call to the UI before results start arriving,
            // otherwise the terminal shows a tool result with no context.
            sink(Event::ToolStart {
                id: id.clone(),
                name: name.clone(),
            });
            // The arguments have to reach the UI too, or the transcript never says what a
            // tool was pointed at: `✓ read 55 lines` is the same line for every file, and
            // two calls in a row are indistinguishable from one call printed twice.
            sink(Event::ToolArgs {
                id: id.clone(),
                args: if arguments.trim().is_empty() {
                    "{}".to_string()
                } else {
                    arguments.clone()
                },
            });
            // Nothing is said here about the arguments, on purpose. `run` reports a call
            // whose arguments do not parse -- and feeds that error back to the model, which
            // is what actually gets it fixed -- so warning here as well says the same thing
            // twice, one line apart. And empty arguments are not a problem to report at all:
            // `list`, `glob` and `grep` take none, and a tool that needs one names the
            // missing argument itself.
            outcome.tool_calls.push(ToolCall {
                id,
                name,
                arguments: if arguments.trim().is_empty() {
                    "{}".to_string()
                } else {
                    arguments
                },
            });
        }

        if let Some(u) = usage_update {
            self.last_usage = Some(u);
            self.record(SessionEvent::Usage { usage: u });
        }

        Ok(outcome)
    }
}

#[derive(Default)]
struct StepOutcome {
    text: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
}

/// How many of the most recent tool results survive pruning.
///
/// The current step's work is what the model is acting on, so that part is never
/// touched. Everything older has already been read and written about, and the model's
/// own summary of it is in the assistant messages, which are kept.
const KEEP_TOOL_RESULTS: usize = 4;

/// Anything this short is worth more than the note that replaces it.
const MIN_PRUNABLE: usize = 160;

/// What the model is asked for when a person compacts the conversation.
///
/// Four things are asked for and every one of them is there because leaving it out is how a summary
/// loses the thing the next question needs: what was decided (not just what was discussed), the names
/// and paths that were settled on, what went wrong, and what is still open. "Do not answer it" is the
/// last line because the request *is* a user message like any other: a model that treats it as the
/// next question replies with the work instead of the summary, and the fold then stores an answer.
const COMPACT_INSTRUCTION: &str = "\
Summarize the conversation above, for a later turn that will read your summary instead of it. \
Keep every fact the work depends on: what was decided and why, the exact names, paths, commands and \
numbers that were settled on, what was tried and failed, and what is still open. Drop the small talk \
and the intermediate steps that led nowhere. Write it as prose in the third person, in the language \
the conversation was held in, and do not answer anything in it -- reply with the summary and nothing else.";

/// What a relayed peer message reads as, or `None` when there is nothing to relay.
///
/// One labelled user message for all of them, and the label is the whole point: a peer's words arriving
/// as though the person had typed them would be a worse lie than the silence the default keeps. It says
/// where they came from, that the person asked for them to be passed on, and that another process wrote
/// them -- because the model's next move may depend on all three, and it cannot tell from the text.
///
/// A free function rather than a method so the wording can be tested without an agent, a provider or a
/// session: it is a sentence, and the sentence is the safety-relevant part.
fn peer_relay(said: &[(String, String)]) -> Option<Message> {
    if said.is_empty() {
        return None;
    }
    let mut block = String::from(
        "[a peer run working in this directory left a message, and the person who started this run \
         asked for peers to be heard; it comes from another process, not from them]",
    );
    for (from, text) in said {
        let who = if from.trim().is_empty() { "someone" } else { from.trim() };
        block.push_str(&format!("\n\n{who} says: {text}"));
    }
    Some(Message::user(&block))
}

/// The sentence that tells the model a job it started has ended and nobody has read what it produced.
///
/// Labelled as flint's own bookkeeping in the first line, because it is the one message in a
/// conversation that neither the person nor the model wrote, and a model that mistakes it for the
/// person speaking would answer it instead of using it. Free-standing for `peer_relay`'s reason: the
/// wording is the part that matters, and it can be tested without an agent, a provider or a session.
fn job_report(lines: &[String]) -> Option<Message> {
    if lines.is_empty() {
        return None;
    }
    let mut block = String::from(
        "[a note from flint, not from the person: work this run started and did not wait for has \
         ended since the last message]",
    );
    for line in lines {
        block.push_str("\n\n");
        block.push_str(line);
    }
    Some(Message::user(&block))
}

/// What the model is asked with: the conversation minus stale tool output.
///
/// Errors are exempt. The one thing a reader -- human or model -- may still have to act
/// on is *why* something failed, and a failure that scrolls out of context is how the
/// same broken command gets tried a fifth time.
///
/// This is a request-side view only. `self.history` and the session file keep every
/// byte, because those are the record of what happened rather than the prompt.
fn prune_tool_output(history: &[Message]) -> Vec<Message> {
    let tool_total = history
        .iter()
        .filter(|m| matches!(m, Message::Tool { .. }))
        .count();
    let mut seen = 0usize;

    history
        .iter()
        .map(|m| match m {
            Message::Tool {
                tool_call_id,
                content,
            } if tool_total.saturating_sub(seen) > KEEP_TOOL_RESULTS => {
                seen += 1;
                if content.len() <= MIN_PRUNABLE || looks_like_failure(content) {
                    m.clone()
                } else {
                    Message::Tool {
                        tool_call_id: tool_call_id.clone(),
                        content: format!(
                            "[{} lines of earlier output dropped to save context; \
                             ask again if you need it]",
                            content.lines().count().max(1)
                        ),
                    }
                }
            }
            Message::Tool { .. } => {
                seen += 1;
                m.clone()
            }
            other => other.clone(),
        })
        .collect()
}

/// Keep a request inside its budget by dropping the oldest whole turns.
///
/// `max_request_chars` is a guard in the spirit of `max_steps`: without it a conversation grows the
/// request without bound until the provider refuses it, and the refusal arrives in the middle of a
/// turn rather than as a decision anybody made. Two things bound a request today and neither of them
/// is this: the step guard bounds *one* turn, and `prune_tool_output` shrinks the stale results
/// inside whatever history is being sent.
///
/// The unit is a **turn** -- a user message through to the next one, which is the smallest piece that
/// can be dropped without leaving the request malformed. A tool result whose call has been dropped is
/// a request the provider rejects, and a turn boundary cannot fall inside such a pair.
///
/// The newest turn is never dropped, whatever the budget says: a request with nothing to answer is
/// not a smaller request, it is a broken one. The same reason keeps the system prompt, which is not
/// counted against the budget at all -- the model has to be told what it is, and a budget that could
/// take it away could empty the request from the wrong end.
///
/// This is a request-side view only. `self.history` and the session file keep every byte, for the
/// reason `prune_tool_output` gives about tool output, and the note left in place of the dropped
/// turns is what makes them recoverable: it says how many went.
fn trim_old_turns(history: &[Message], budget: usize) -> Vec<Message> {
    // 0 is off, the way an empty `proxy` is off: a person who does not want the guard says so in the
    // file rather than by deleting the line and getting the default back on the next write.
    if budget == 0 {
        return history.to_vec();
    }

    // A turn starts at a user message. Everything before the first one is the prompt -- the system
    // message, and on a loaded history possibly nothing else -- and a history with no user message at
    // all is one unit, because there is no boundary to cut on.
    let first_turn = history
        .iter()
        .position(|m| matches!(m, Message::User { .. }))
        .unwrap_or(history.len());
    let starts: Vec<usize> = (first_turn..history.len())
        .filter(|&i| matches!(history[i], Message::User { .. }))
        .collect();
    if starts.is_empty() {
        return history.to_vec();
    }
    let turn = |index: usize| -> &[Message] {
        let end = starts.get(index + 1).copied().unwrap_or(history.len());
        &history[starts[index]..end]
    };
    let turn_chars = |index: usize| -> usize { turn(index).iter().map(message_chars).sum() };

    // Newest first, while they fit. The newest turn is taken whatever it costs: it is the one the
    // request is for, and a request that asks nothing is not a smaller request.
    let mut from = starts.len() - 1;
    let mut used = turn_chars(from);
    while from > 0 && used + turn_chars(from - 1) <= budget {
        from -= 1;
        used += turn_chars(from);
    }
    if from == 0 {
        return history.to_vec();
    }

    // The note is part of the request, so it is charged to the budget like anything else is. Dropping
    // one more turn is how it fits, and the newest turn is still never dropped for it.
    loop {
        let dropped = starts[from] - first_turn;
        let note = message_chars(&dropped_turns_note(dropped, budget));
        if used + note <= budget || from + 1 == starts.len() {
            break;
        }
        from += 1;
        used -= turn_chars(from - 1);
    }

    let mut view: Vec<Message> = history[..first_turn].to_vec();
    view.push(dropped_turns_note(starts[from] - first_turn, budget));
    for index in from..starts.len() {
        view.extend_from_slice(turn(index));
    }
    view
}

/// What one message costs the request, in characters.
///
/// Characters rather than tokens, because characters are what flint can count without a tokenizer:
/// the same unit `max_tool_output` uses, and the unit the config key is in. The number that matters
/// is a ceiling -- the provider's context -- and being a fifth wrong about it is fine when the
/// budget is chosen with room to spare.
fn message_chars(message: &Message) -> usize {
    /// What a message costs beyond its text: the role, the ids, and the JSON around it.
    const OVERHEAD: usize = 16;
    let text = match message {
        Message::System { content } | Message::User { content } => content.chars().count(),
        Message::Tool { content, .. } => content.chars().count(),
        Message::Assistant {
            content,
            reasoning,
            tool_calls,
        } => {
            content.as_deref().unwrap_or_default().chars().count()
                + reasoning.as_deref().unwrap_or_default().chars().count()
                + tool_calls
                    .iter()
                    .map(|call| {
                        call.id.chars().count()
                            + call.name.chars().count()
                            + call.arguments.chars().count()
                    })
                    .sum::<usize>()
        }
    };
    text + OVERHEAD
}

/// What the model is told where the dropped turns were.
///
/// A message rather than silence: shown a conversation that starts mid-way with no explanation, a
/// model treats the first thing it sees as the whole of what happened and answers as if the earlier
/// work never occurred. The count belongs here too, because "some context is missing" without a
/// number is a fact the model cannot act on.
fn dropped_turns_note(dropped: usize, budget: usize) -> Message {
    Message::user(format!(
        "[{dropped} earlier {} left out of this request: the conversation is longer than \
         max_request_chars ({budget}). The session file keeps every one of them, so ask for \
         anything older by name, or read the file.]",
        if dropped == 1 {
            "message was"
        } else {
            "messages were"
        }
    ))
}

/// Whether a tool result reports that it did not work.
///
/// Matched loosely on purpose: the wording comes from a shell, a language runtime, or
/// an HTTP client, and none of those were written with this check in mind.
fn looks_like_failure(content: &str) -> bool {
    let head: String = content.chars().take(400).collect::<String>().to_lowercase();
    head.contains("[exit code:")
        || head.contains("error")
        || head.contains("failed")
        || head.contains("cannot")
        || head.contains("not found")
        || head.contains("denied")
        || head.contains("no such file")
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_result(id: &str, content: &str) -> Message {
        Message::Tool {
            tool_call_id: id.to_string(),
            content: content.to_string(),
        }
    }

    fn big(n: usize) -> String {
        (0..n)
            .map(|i| format!("entry-{i} some listing text"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The words the model is given when a peer is heard, which are the only thing standing between a
    /// relayed message and a model that thinks the person typed it.
    #[test]
    fn a_relayed_peer_message_says_who_wrote_it_and_that_the_person_let_it_through() {
        assert!(peer_relay(&[]).is_none(), "nothing said is nothing to relay");

        let relay = peer_relay(&[
            ("peer 7".to_string(), "do not commit docs/sandbox.md".to_string()),
            (String::new(), "the tree is yours".to_string()),
        ])
        .expect("a relay");
        let Message::User { content } = &relay else {
            panic!("a peer's words have to arrive as a user message: {relay:?}");
        };
        assert!(content.contains("peer 7"), "{content}");
        assert!(content.contains("do not commit docs/sandbox.md"), "{content}");
        assert!(content.contains("someone says: the tree is yours"), "{content}");
        assert!(
            content.contains("another process, not from them"),
            "the relay does not say it is not the person's own words: {content}"
        );
        assert!(
            content.contains("asked for peers to be heard"),
            "the relay does not say the person allowed it: {content}"
        );
    }

    /// A turnaround that runs to the step limit collects one tool result per step. Once
    /// they are no longer the thing being worked on, re-sending them all is what walks a
    /// working turn into the context ceiling.
    #[test]
    fn stale_tool_output_is_dropped_from_the_request() {
        let mut history = vec![Message::user("fix codex")];
        for i in 0..12 {
            history.push(tool_result(&format!("call_{i}"), &big(30)));
        }

        let sent = prune_tool_output(&history);
        let kept: Vec<&Message> = sent
            .iter()
            .filter(|m| matches!(m, Message::Tool { .. }))
            .collect();
        assert_eq!(kept.len(), 12, "messages must not disappear, only shrink");

        let last = match kept.last().unwrap() {
            Message::Tool { content, .. } => content,
            _ => unreachable!(),
        };
        assert!(last.contains("entry-0"), "recent output was pruned: {last}");
        let first = match kept.first().unwrap() {
            Message::Tool { content, .. } => content,
            _ => unreachable!(),
        };
        assert!(!first.contains("entry-0"), "stale output was kept: {first}");
        assert!(first.contains("dropped to save context"), "{first}");
    }

    /// The failure is the part still worth having: it is what stops the same broken
    /// command being tried a fifth time.
    #[test]
    fn an_old_failure_is_never_pruned() {
        let mut history = vec![Message::user("go")];
        history.push(tool_result("call_old", &format!("[exit code: 1]\n{}", big(40))));
        for i in 0..6 {
            history.push(tool_result(&format!("call_{i}"), &big(30)));
        }

        let sent = prune_tool_output(&history);
        let kept = match &sent[1] {
            Message::Tool { content, .. } => content,
            other => panic!("expected a tool message, got {other:?}"),
        };
        assert!(kept.contains("[exit code: 1]"), "the failure was pruned: {kept}");
        assert!(kept.contains("entry-0"), "the detail was pruned: {kept}");
    }

    /// Below the threshold there is nothing to gain: the note would be as long as the
    /// thing it replaces.
    #[test]
    fn a_short_result_is_left_alone() {
        let mut history = vec![Message::user("go")];
        history.push(tool_result("call_old", "3 lines"));
        for i in 0..6 {
            history.push(tool_result(&format!("call_{i}"), &big(30)));
        }

        let sent = prune_tool_output(&history);
        match &sent[1] {
            Message::Tool { content, .. } => assert_eq!(content, "3 lines"),
            other => panic!("expected a tool message, got {other:?}"),
        }
    }

    fn cfg_for_prompt() -> Config {
        Config::default()
    }

    /// A message the model said, which is what most of a conversation is after the first turn.
    fn said(text: &str) -> Message {
        Message::Assistant {
            content: Some(text.to_string()),
            reasoning: None,
            tool_calls: Vec::new(),
        }
    }

    /// A call the model made, which has to stay with the results that answer it.
    fn called(id: &str) -> Message {
        Message::Assistant {
            content: None,
            reasoning: None,
            tool_calls: vec![crate::event::ToolCall {
                id: id.to_string(),
                name: "bash".to_string(),
                arguments: "{}".to_string(),
            }],
        }
    }

    /// Every tool result in a view answers a call the same view still carries.
    ///
    /// The provider refuses a request where one does not, so this is the property the trim has to
    /// keep rather than trust luck about; it is checked in the tests that drop turns near a pair.
    fn every_result_answers_a_kept_call(view: &[Message]) -> bool {
        let mut calls: Vec<&str> = Vec::new();
        for message in view {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    calls.extend(tool_calls.iter().map(|c| c.id.as_str()));
                }
                Message::Tool { tool_call_id, .. } if !calls.contains(&tool_call_id.as_str()) => {
                    return false;
                }
                _ => {}
            }
        }
        true
    }

    /// A conversation, unlike a single turn, had nothing bounding it: the step guard covers one turn
    /// and pruning covers the stale results inside it, so a long conversation kept growing the
    /// request until the provider refused it mid-turn.
    #[test]
    fn a_conversation_over_its_budget_loses_its_oldest_turns_and_is_told_so() {
        let mut history = vec![Message::system("you are flint")];
        for i in 0..20 {
            history.push(Message::user(format!("question {i} {}", "x".repeat(100))));
            history.push(said(&format!("answer {i} {}", "y".repeat(100))));
        }

        let sent = trim_old_turns(&history, 1000);

        assert_eq!(
            history.len(),
            41,
            "the trim must not touch the history it is given"
        );
        assert!(
            matches!(sent.first(), Some(Message::System { .. })),
            "the system prompt is not a turn and is never dropped: {sent:?}"
        );
        let Message::User { content: note } = &sent[1] else {
            panic!("the dropped turns must be replaced by a note: {sent:?}");
        };
        assert!(
            note.contains("left out of this request"),
            "the note does not say what happened: {note}"
        );
        // Counted from what is actually in the view rather than from its length, so a note that
        // names the wrong number cannot pass: a turn is two messages, and a kept answer means its
        // question was kept too. The trailing space matters -- without it `answer 1` matches
        // `answer 19`, which is how this check first counted four turns where three were kept.
        let kept = (0..20)
            .filter(|i| {
                let answer = format!("answer {i} ");
                sent.iter().any(|m| format!("{m:?}").contains(&answer))
            })
            .count();
        assert!(kept < 20, "nothing was dropped at all: {sent:?}");
        assert!(
            note.contains(&format!("{} earlier messages", 2 * (20 - kept))),
            "the note does not say how many went ({kept} turns of 20 are still here): {note}"
        );
        assert!(
            !sent.iter().any(|m| format!("{m:?}").contains("question 0")),
            "the oldest turn is still in the request"
        );
        assert!(
            format!("{:?}", sent.last().unwrap()).contains("answer 19"),
            "the newest turn is what the request is for"
        );

        let conversation: usize = sent[1..].iter().map(message_chars).sum();
        assert!(
            conversation <= 1000,
            "the request is over its budget with the note in it: {conversation} characters"
        );
        assert!(every_result_answers_a_kept_call(&sent));
    }

    /// The pair is the unit: a result whose call has been dropped is a request the provider refuses.
    #[test]
    fn a_tool_call_is_never_dropped_without_its_results() {
        let mut history = vec![Message::system("you are flint")];
        history.push(Message::user("old question"));
        history.push(called("call_old"));
        history.push(tool_result("call_old", &big(400)));
        history.push(said("that was the old answer"));
        history.push(Message::user("new question"));
        history.push(called("call_new"));
        history.push(tool_result("call_new", "a few lines"));

        // A budget too small for even the newest turn: the turn is still sent whole, because half of
        // a call-and-result pair is a request the provider refuses.
        let sent = trim_old_turns(&history, 50);

        assert!(
            !sent.iter().any(|m| format!("{m:?}").contains("call_old")),
            "the old call and its result should both be gone: {sent:?}"
        );
        assert!(
            sent.iter().any(|m| format!("{m:?}").contains("call_new")),
            "the newest turn was dropped: {sent:?}"
        );
        assert!(
            sent.iter().filter(|m| matches!(m, Message::Tool { .. })).count() == 1,
            "a result survived its call, or a call survived without one: {sent:?}"
        );
        assert!(every_result_answers_a_kept_call(&sent));
    }

    /// Below the budget there is nothing to do, and a note would be a lie about a request that is
    /// complete.
    #[test]
    fn a_conversation_that_fits_is_sent_unchanged() {
        let mut history = vec![Message::system("you are flint")];
        history.push(Message::user("one question"));
        history.push(said("one answer"));

        let sent = trim_old_turns(&history, 400_000);
        assert_eq!(sent.len(), history.len(), "something was dropped: {sent:?}");
        assert!(
            !sent.iter().any(|m| format!("{m:?}").contains("left out of this request")),
            "a note was added to a request nothing was dropped from: {sent:?}"
        );
    }

    /// Whatever the budget says, the turn being asked is in the request: a request with nothing to
    /// answer is not a smaller request, it is a broken one.
    #[test]
    fn the_newest_turn_is_never_dropped() {
        let mut history = vec![Message::system("you are flint")];
        history.push(Message::user("old ".repeat(500)));
        history.push(said(&"old ".repeat(500)));
        history.push(Message::user("the question being asked"));
        history.push(said("part of the answer already drawn"));

        for budget in [1, 50, 200] {
            let sent = trim_old_turns(&history, budget);
            assert!(
                sent.iter().any(|m| format!("{m:?}").contains("the question being asked")),
                "a budget of {budget} dropped the question: {sent:?}"
            );
            assert!(
                sent.iter().any(|m| format!("{m:?}").contains("part of the answer")),
                "a budget of {budget} dropped the turn in flight: {sent:?}"
            );
        }
    }

    /// The key's unit is characters, like `max_tool_output`, and 0 is how a person turns the guard
    /// off without deleting the line.
    #[test]
    fn a_budget_of_zero_sends_the_whole_conversation() {
        let mut history = vec![Message::system("you are flint")];
        for i in 0..30 {
            history.push(Message::user(format!("question {i} {}", "x".repeat(100))));
            history.push(said(&format!("answer {i}")));
        }

        let sent = trim_old_turns(&history, 0);
        assert_eq!(sent.len(), history.len(), "0 is off, not a request of nothing");
    }

    /// The prompt must carry the shell dialect for the platform it was built
    /// for, and must not hedge by describing both.
    /// Every built-in tool that has a shell equivalent has to be named as preferred.
    ///
    /// Found by a real run: a local model asked to read a URL reached for `bash` and `curl`,
    /// because the paragraph that tells it to prefer the built-in tools listed the file tools
    /// and not `fetch`. A schema says what a tool does; only the prompt says to reach for it,
    /// and a tool nobody is told to prefer is a tool that goes unused.
    #[test]
    fn the_prompt_names_the_tools_it_wants_preferred_over_the_shell() {
        let p = build_system_prompt(&cfg_for_prompt(), std::path::Path::new("/tmp"));
        for tool in ["read", "write", "edit", "list", "glob", "grep", "exec", "fetch"] {
            assert!(
                p.contains(&format!("`{tool}`")),
                "the prompt never names `{tool}` as a tool to prefer: {p}"
            );
        }
        // And the shell equivalents it should be replacing are named too, so the sentence is
        // an instruction rather than a list.
        for equivalent in ["curl", "find"] {
            assert!(p.contains(equivalent), "the prompt does not mention {equivalent}: {p}");
        }
    }

    #[test]
    fn system_prompt_states_the_platform_shell() {
        let cwd = std::path::Path::new("/tmp/example");
        let p = build_system_prompt(&cfg_for_prompt(), cwd);

        assert!(p.contains("Working directory"));
        assert!(p.contains("example"), "cwd must appear: {p}");

        if cfg!(windows) {
            assert!(
                p.contains("cmd.exe"),
                "a Windows build must warn about cmd.exe"
            );
            assert!(
                p.contains("`dir`"),
                "it must name the replacement command, not just refuse `ls`"
            );
            assert!(
                p.contains("PowerShell (use the `pwsh` tool):"),
                "a Windows build must say which PowerShell it is talking to: {p}"
            );
            assert!(
                !p.contains("POSIX shell: `ls`"),
                "must not advertise POSIX tools on Windows"
            );
        } else {
            assert!(
                p.contains("POSIX shell"),
                "a Unix build must say the shell is POSIX"
            );
            assert!(!p.contains("cmd.exe"), "must not mention cmd.exe on Unix");
        }
    }

    /// The PowerShell facts are measured from the machine, not written into the binary.
    ///
    /// A version baked into a constant would be a lie on the next machine, and a lie that is
    /// only discovered by writing a script that will not parse. Asserted on the shape rather
    /// than on the number: this has to keep working on a machine with PowerShell 7.
    #[cfg(windows)]
    #[test]
    fn the_powershell_facts_come_from_the_machine() {
        let facts =
            crate::tools::powershell_facts().expect("Windows ships with Windows PowerShell");
        assert!(
            facts.starts_with("pwsh ") || facts.starts_with("powershell "),
            "the facts must name the program that answered: {facts}"
        );
        assert!(
            facts.contains("arguments to native commands:"),
            "the argument-passing mode is one of the two reasons this exists: {facts}"
        );
        assert!(
            facts.chars().any(|c| c.is_ascii_digit()) && facts.contains('.'),
            "there is no version in {facts}"
        );
    }

    /// The task must come from the user, not from the prompt.
    ///
    /// Reported from real sessions: every request came back framed as a repair. The cause
    /// was not the model but the prompt, which said "your job is to diagnose and repair
    /// it" and "assume the user's usual tooling may be broken". A prompt that fixes the
    /// agent's purpose turns "write me a script" into an inspection of a system that was
    /// never broken -- and spends the turn looking for damage nobody mentioned.
    ///
    /// So the prompt describes *capabilities and constraints* and leaves the objective to
    /// the request. This test pins that: the phrasing that caused it must not come back.
    #[test]
    fn system_prompt_does_not_dictate_the_task() {
        let p = build_system_prompt(&cfg_for_prompt(), std::path::Path::new("/tmp"));
        for banned in [
            "your job is to",
            "diagnose and repair",
            "fallback tool",
            "usual tooling may be broken",
        ] {
            assert!(
                !p.to_lowercase().contains(banned),
                "the prompt fixes the agent's purpose with {banned:?}, which makes every \
                 request a repair: {p}"
            );
        }
        assert!(
            p.contains("The task is whatever the user asks for"),
            "the prompt must say the objective comes from the request: {p}"
        );
    }

    /// flint must know where its own configuration lives.
    ///
    /// Reported from a real session: asked to change its model, flint went looking through
    /// the project's source. It is an installed binary; the model and providers are config,
    /// not code, and the prompt has to say so -- including the path, which the model cannot
    /// guess and should not have to search for.
    #[test]
    fn system_prompt_says_where_flint_is_configured() {
        let p = build_system_prompt(&cfg_for_prompt(), std::path::Path::new("/tmp"));
        assert!(
            p.contains(&crate::config::config_path().display().to_string()),
            "the config path must be stated outright: {p}"
        );
        assert!(
            p.to_lowercase().contains("config"),
            "it must say the settings are configuration rather than code: {p}"
        );
        assert!(
            p.contains("/model") || p.contains("/provider"),
            "it must name the commands that change those settings: {p}"
        );
    }

    /// A rescue tool that restates the obvious is burning the user's context.
    #[test]
    fn system_prompt_does_not_waste_tokens_on_the_obvious() {
        let p = build_system_prompt(&cfg_for_prompt(), std::path::Path::new("/tmp"));
        assert!(
            !p.contains("OS: "),
            "the binary knows its own OS; saying so only costs tokens"
        );
        assert!(
            !p.contains("ARCH") && !p.contains("x86_64") && !p.contains("aarch64"),
            "architecture is likewise known at compile time"
        );
    }
}
