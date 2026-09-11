//! The agent loop.
//!
//! One iteration is: ask the provider -> collect streamed events -> if the
//! model requested tools, run them and feed the results back -> repeat until it
//! answers without asking for a tool, or the step cap is reached.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::Config;
use crate::event::{Event, Message, ToolCall, Usage};
use crate::provider::Provider;
use crate::session::{SessionEvent, SessionWriter};
use crate::tools::{self, ToolBox};

pub const SYSTEM_PROMPT: &str = "\
You are flint, a minimal command-line coding and system-repair agent.

You are the fallback tool. Assume the user's usual tooling may be broken; your \
job is to diagnose and repair it, not to be pleasant about it.

Rules:
- Act, do not narrate. Use the tools to inspect the real system before making \
  claims about it.
- Read a file before editing it. Never guess at its contents.
- Prefer small, verifiable steps over large changes.
- When a command fails, read the error and adapt. Do not repeat the same \
  failing command.
- Report exactly what you changed and why.
- Be concise. The user is looking at a terminal.
- Prefer the `read`, `write`, `edit` and `list` tools for files over shell \
  builtins: they behave identically on every platform.";

/// Build the system prompt with the facts the model cannot guess.
///
/// Deliberately short. The binary already knows its own OS and architecture, so
/// stating them spends tokens on something the model cannot act on differently.
/// What it *cannot* infer is the shell dialect -- trying `ls` in cmd.exe is the
/// single most common way this agent wastes a turn -- and where commands will
/// run, because the working directory is set at run time.
pub fn build_system_prompt(config: &Config, cwd: &std::path::Path) -> String {
    let shell = tools::probe_shell(&config.shell, &config.shell_args).join(" ");

    // Only say something when there is something worth knowing.
    let dialect = if cfg!(windows) {
        "It is cmd.exe, not a POSIX shell: use `dir`, `type`, `echo %CD%`. \
         `ls`, `pwd`, `cat`, `find` and `grep` do not exist."
    } else {
        "It is a POSIX shell: `ls`, `cat`, `find` and `grep` are available."
    };

    format!(
        "{SYSTEM_PROMPT}\n\n\
         Local facts:\n\
         - Shell: `{shell}`. {dialect}\n\
         - Working directory: {cwd} (path separator `{sep}`).",
        cwd = cwd.display(),
        sep = std::path::MAIN_SEPARATOR,
    )
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

    /// Run one user turn to completion, reporting progress through `sink`.
    pub async fn run(&mut self, user_input: &str, mut sink: impl FnMut(Event)) -> Result<()> {
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

        {
            let history = &self.history;
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

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}
