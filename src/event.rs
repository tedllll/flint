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
    /// How much of the prompt the provider served out of its own cache, when it says.
    ///
    /// `Option` rather than a plain number, because "this provider does not report caching" and
    /// "this provider reported no hits" are different answers and only the second is a fact about
    /// the prompt flint builds. A `0` printed for the first would be a lie that reads exactly like
    /// the defect the number exists to expose.
    ///
    /// Two shapes are read into this one field (see `provider::usage_from`): DeepSeek's
    /// `prompt_cache_hit_tokens` and OpenAI's `prompt_tokens_details.cached_tokens`. Both count a
    /// subset of `prompt_tokens`, which is what makes the rate below one rule on either endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_tokens: Option<u64>,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// The share of the prompt that came out of the provider's cache, as a whole percent.
    ///
    /// `None` when the provider said nothing about caching, and `None` for a prompt of no tokens --
    /// there is no rate to give for a request that sent nothing, and the division would panic. The
    /// result is bounded at 100 because a provider reporting more hits than prompt tokens is simply
    /// wrong: repeating that as 130% would spread the mistake to anything reading it.
    pub fn cache_rate(&self) -> Option<u64> {
        let hit = self.cache_hit_tokens?;
        if self.prompt_tokens == 0 {
            return None;
        }
        Some((hit.min(self.prompt_tokens) * 100 + self.prompt_tokens / 2) / self.prompt_tokens)
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
    /// A peer left a message for this run, and it is **for the person, not for the model** -- unless
    /// this run was started with `--hear-peers`, which `heard` is.
    ///
    /// `from` is a claim, not an identity: whoever can write the mailbox can write that field, and a
    /// run that hears peers is trusting that claim on purpose. By default this event exists so the
    /// message reaches the transcript and the session file and never a request body, which is the rule
    /// `docs/agents.md` calls the reason the mailbox is safe to have at all; `heard` is the deliberate
    /// exception, and it is on the event so the transcript can say which one happened instead of
    /// telling the person the model has not seen something it has.
    Peer {
        from: String,
        text: String,
        heard: bool,
    },
    /// A non-fatal problem worth surfacing to the user.
    Warning(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt_tokens: u64, cache_hit_tokens: Option<u64>) -> Usage {
        Usage {
            prompt_tokens,
            completion_tokens: 0,
            cache_hit_tokens,
        }
    }

    /// The rate is a reading of two numbers and has to refuse to give one when either is missing.
    ///
    /// Three ways to be wrong, all of them plausible implementations: treating "not reported" as
    /// zero (which accuses the prompt of being unstable on a provider that never said), dividing by
    /// a prompt of no tokens (which panics on the first request of a run pointed at an endpoint that
    /// counts differently), and passing a provider's bad arithmetic through as 130%.
    #[test]
    fn a_cache_rate_needs_both_numbers_and_stays_a_percentage() {
        assert_eq!(usage(1000, None).cache_rate(), None);
        assert_eq!(usage(0, Some(0)).cache_rate(), None);
        assert_eq!(usage(1000, Some(0)).cache_rate(), Some(0));
        assert_eq!(usage(1000, Some(871)).cache_rate(), Some(87));
        assert_eq!(usage(1000, Some(875)).cache_rate(), Some(88));
        assert_eq!(usage(1000, Some(1000)).cache_rate(), Some(100));
        assert_eq!(usage(10, Some(999)).cache_rate(), Some(100));
    }

    /// A usage the provider never mentioned must not serialise the field at all.
    ///
    /// The session file is the record, and an absent field is what lets a reader tell an endpoint
    /// that reports caching from one that does not -- `docs/session-format.md` says so in those
    /// words, and a `0` written here would make both files identical.
    #[test]
    fn a_usage_without_a_cache_split_says_nothing_about_caching() {
        let line = serde_json::to_string(&usage(1000, None)).expect("serialise");
        assert!(!line.contains("cache"), "{line}");
        let back: Usage = serde_json::from_str(r#"{"prompt_tokens":5,"completion_tokens":1}"#)
            .expect("an old line still reads");
        assert_eq!(back.cache_hit_tokens, None);
        assert_eq!(back.total(), 6);
    }
}
