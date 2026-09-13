//! Unified streaming events.
//!
//! Everything the provider produces and everything the agent does flows through
//! this one enum. The UI layer only needs to understand these variants, which
//! keeps provider-specific quirks contained behind the `Provider` trait.

use serde::{Deserialize, Serialize};

/// A single item in the conversation, in provider-neutral form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Message::User {
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }
}

/// A model's request to invoke a tool.
///
/// Internally the name and arguments live flat, because that is convenient for
/// the agent. On the wire, though, the OpenAI schema nests them:
///
/// ```json
/// {"id":"call_1","type":"function","function":{"name":"bash","arguments":"{}"}}
/// ```
///
/// `arguments` stays a raw JSON *string* because that is exactly how providers
/// stream it (in fragments); we only parse once the fragments are complete.
/// Both representations are kept in sync deliberately: `name`/`arguments` are
/// skipped when serialising so that only the nested form is ever sent, while
/// `function` is skipped when deserialising so the flat fields are what code
/// reads.
#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Serialize for ToolCall {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ToolCall", 3)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("type", "function")?;
        s.serialize_field(
            "function",
            &ToolCallFunction {
                name: &self.name,
                arguments: &self.arguments,
            },
        )?;
        s.end()
    }
}

#[derive(Serialize)]
struct ToolCallFunction<'a> {
    name: &'a str,
    arguments: &'a str,
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Accept the nested wire shape.
        #[derive(Deserialize)]
        struct Nested {
            #[serde(default)]
            id: String,
            function: Option<Function>,
        }
        #[derive(Deserialize)]
        struct Function {
            #[serde(default)]
            name: String,
            #[serde(default)]
            arguments: String,
        }

        let nested = Nested::deserialize(deserializer)?;
        let function = nested.function.unwrap_or(Function {
            name: String::new(),
            arguments: String::new(),
        });
        Ok(ToolCall {
            id: nested.id,
            name: function.name,
            arguments: function.arguments,
        })
    }
}

/// Token accounting, straight from the provider when it reports one.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Events emitted while a single agent turn is being executed.
#[derive(Debug, Clone)]
pub enum Event {
    /// Visible assistant text.
    Text(String),
    /// Reasoning/thinking text, when the model exposes it (DeepSeek R1, etc).
    Reasoning(String),
    /// The model has decided to call a tool.
    ToolStart { id: String, name: String },
    /// Raw tool arguments (complete JSON string, suitable for display).
    ToolArgs { id: String, args: String },
    /// A tool finished; `ok` is false when it returned an error.
    ToolResult {
        id: String,
        output: String,
        ok: bool,
    },
    /// Token usage reported by the provider.
    Usage(Usage),
    /// What the turn is waiting for has changed.
    ///
    /// The terminal's status row and the browser's are the same fact, and this is the only
    /// place it is decided: `run_turn` emits it at the four moments the phase changes, and
    /// every renderer displays it. Without it a browser shows nothing at all between
    /// `turn.started` and the first delta -- which with a local model is minutes, and reads
    /// as a program that has hung.
    ///
    /// `restarted` is whether this begins a **new** wait, as opposed to renaming the one
    /// already running. A renderer with an elapsed-time clock starts it over only then:
    /// `waiting for the model` becoming `writing the answer` thirty seconds in is the same
    /// wait, and resetting the clock would show `0s` where the terminal shows `30s`.
    Status { text: String, restarted: bool },
    /// One agent turn finished (no more tool calls pending).
    Done,
    /// A non-fatal problem worth surfacing to the user.
    Warning(String),
}
