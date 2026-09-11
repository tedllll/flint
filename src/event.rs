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

/// A model's request to invoke a tool. Arguments travel as a raw JSON string
/// because that is exactly how providers stream them (in fragments); we only
/// parse once the fragments are complete.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
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
    /// One agent turn finished (no more tool calls pending).
    Done,
    /// A non-fatal problem worth surfacing to the user.
    Warning(String),
}
