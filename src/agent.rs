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
pub fn build_system_prompt(config: &Config, cwd: &std::path::Path) -> String {
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

    // What the project has written down. Discovered here, once per agent, because the
    // answer depends on the working directory -- and rebuilt by `/model` and friends,
    // which is how a file added mid-session gets noticed without a restart.
    let mode =
        context::Instructions::parse(&config.instructions).unwrap_or(context::Instructions::Hint);
    let note = context::Workspace::discover(cwd, &config.skill_dirs).prompt_note(mode);
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
        let tools = ToolBox::new(config, readonly, cwd.clone());
        Agent {
            provider,
            tools,
            history: vec![Message::system(build_system_prompt(config, &cwd))],
            max_steps: config.max_steps,
            readonly,
            cwd,
            writer,
            last_usage: None,
        }
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

            for call in &outcome.tool_calls {
                let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        let msg = format!(
                            "invalid JSON arguments for tool '{}': {e}. Raw: {}",
                            call.name,
                            crate::util::truncate(&call.arguments, 400)
                        );
                        sink(Event::Warning(msg.clone()));
                        self.push_tool_result(call, &msg, false);
                        continue;
                    }
                };

                let result = self.tools.invoke(&call.name, &args).await;
                match result {
                    Ok(output) => {
                        let ok = !output.contains("[exit code:");
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
            if !arguments.trim().is_empty() {
                if serde_json::from_str::<serde_json::Value>(&arguments).is_err() {
                    sink(Event::Warning(format!(
                        "tool '{name}' returned malformed argument JSON: {}",
                        crate::util::truncate(&arguments, 200)
                    )));
                }
            } else {
                sink(Event::Warning(format!(
                    "tool '{name}' was requested with no arguments"
                )));
            }
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
