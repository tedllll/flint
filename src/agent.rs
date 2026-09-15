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
    readonly: bool,
    cwd: PathBuf,
    writer: Option<SessionWriter>,
    last_usage: Option<Usage>,
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
        // knows which run produced them. One-shot mode has no session to belong to.
        let tag = writer
            .as_ref()
            .and_then(|w| w.path().file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unattached".to_string());
        let tools = ToolBox::new(config, readonly, cwd.clone())
            .with_spill_dir(crate::config::spill_dir().join(tag));
        // One walk, two answers: the prompt's note and the page's menu. See `Agent::skills`.
        let workspace = context::Workspace::discover(&cwd, &config.skill_dirs);
        let skills = workspace.skill_names();
        Agent {
            provider,
            tools,
            history: vec![Message::system(prompt_with_workspace(config, &cwd, &workspace))],
            max_steps: config.max_steps,
            readonly,
            cwd,
            writer,
            last_usage: None,
            repeats: std::collections::HashMap::new(),
            drawn: String::new(),
            skills,
        }
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

    pub fn readonly(&self) -> bool {
        self.readonly
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
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
        crate::provider::request_body(
            self.provider.model(),
            &prune_tool_output(&history),
            &self.tools.specs(),
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
        let mut merged = vec![Message::system(prompt_with_workspace(cfg, cwd, &workspace))];
        merged.extend(
            loaded
                .into_iter()
                .filter(|m| !matches!(m, Message::System { .. })),
        );
        self.history = merged;
        // The prompt was rebuilt, so the menu the page draws is rebuilt with it: a page offering a
        // skill this run's prompt no longer mentions would be promising more than the model has.
        self.skills = workspace.skill_names();
    }

    /// The skills this run was given, in the order the model sees them.
    pub fn skills(&self) -> &[String] {
        &self.skills
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
        match &self.writer {
            Some(writer) => writer.title(name),
            None => anyhow::bail!("this conversation is not being saved"),
        }
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

            for call in &outcome.tool_calls {
                let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
                    Ok(v) => v,
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

                let result = self.tools.invoke(&call.name, &args).await;
                match result {
                    Ok(output) => {
                        let ok = !output.contains("[exit code:");
                        let output = match self.note_repeat(&call.name, &args) {
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
                    let placeholder = Message::Tool {
                        tool_call_id: call.id.clone(),
                        content: format!(
                            "interrupted by the user: tool '{}' was requested but never ran.",
                            call.name
                        ),
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
                eprintln!("flint: warning: cannot persist session event: {e:#}");
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
