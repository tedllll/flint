//! Provider layer: one OpenAI-compatible streaming client.
//!
//! Every mainstream endpoint (DeepSeek, Kimi, GLM, OpenRouter, Together,
//! Ollama, vLLM, llama.cpp) speaks this protocol, so a single implementation
//! covers all of them. Native protocols (Anthropic's `/v1/messages`) are
//! deliberately out of scope for the rescue tool.
//!
//! The two things that actually bite here, and which the tests below pin down:
//!   1. SSE frames split across network chunks — handled by keeping a
//!      carry-over buffer and only consuming complete lines.
//!   2. `tool_calls[].function.arguments` arrives as string *fragments* that
//!      must be concatenated by index before the JSON is valid.
//!
//! `StreamParser` is deliberately a plain, synchronous state machine with no
//! I/O so it can be tested against recorded provider payloads.

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::ProviderConfig;
use crate::event::{Event, Message, ToolCall, Usage};

#[derive(Clone)]
pub struct Provider {
    config: ProviderConfig,
    client: reqwest::Client,
}

/// Which proxy to reach this provider through -- an explicit one, or none.
///
/// "None" is the default and it means *direct*, not "whatever the platform thinks".
/// reqwest otherwise picks up the Windows system proxy from the registry, which is a
/// setting the user may not have chosen and cannot see from flint: on a machine where a
/// proxy client is installed but has no server selected, `ProxyEnable=1` points at a
/// closed port and every request dies inside a tunnel nothing owns. The symptom is a
/// working network and an agent that cannot connect -- with no configured proxy to
/// blame, because there is not one.
///
/// So the only proxy is the one written down for this provider. An explicit setting is
/// honoured; silence means connect directly.
pub fn configured_proxy(config: &ProviderConfig) -> Option<String> {
    config
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
}

/// Whether a base URL points at a local model server.
///
/// Used to keep local endpoints off any configured proxy: the local provider is
/// the one that still works when the network is what broke, so it must not
/// depend on the network path being healthy.
pub fn is_local_endpoint(base_url: &str) -> bool {
    let u = base_url.to_ascii_lowercase();
    u.contains("localhost")
        || u.contains("127.0.0.1")
        || u.contains("0.0.0.0")
        || u.contains("[::1]")
}

/// Guarantee the one shape the API insists on: every tool call an assistant message
/// asked for is answered by a tool message right after it.
///
/// The agent repairs its own history, so this should have nothing left to do. It runs
/// anyway because the cost of the API rejecting a request is the whole session being
/// unusable, and the check is cheap; a provider that only sometimes produces valid
/// payloads is not one you can rescue a broken machine with.
fn ensure_tool_calls_are_answered(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = messages[index].clone();
        out.push(message.clone());
        let Message::Assistant { tool_calls, .. } = &message else {
            index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            index += 1;
            continue;
        }
        let mut answered: Vec<String> = Vec::new();
        let mut next = index + 1;
        while let Some(Message::Tool { tool_call_id, .. }) = messages.get(next) {
            answered.push(tool_call_id.clone());
            out.push(messages[next].clone());
            next += 1;
        }
        for call in tool_calls {
            if !answered.contains(&call.id) {
                out.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!(
                        "interrupted by the user: tool '{}' was requested but never ran.",
                        call.name
                    ),
                });
            }
        }
        index = next;
    }
    out
}

impl Provider {
    /// A client that is valid but points nowhere, for when the real one cannot be built.
    ///
    /// Used so a failure to configure the client does not stop the process from opening
    /// its own session history. Nothing is ever sent through it: the deferred error is
    /// reported before the first request.
    pub fn fallback_config() -> ProviderConfig {
        ProviderConfig {
            name: "unavailable".to_string(),
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: String::new(),
            model: String::new(),
            api_key_env: None,
            proxy: None,
        }
    }

    /// One line naming the proxy in force, for an error message.
    ///
    /// Only ever present when one was configured, so its absence is a real signal: the
    /// request went out directly. Silence is the useful default here -- there is nothing
    /// to say about a proxy when there is not one.
    fn proxy_note(&self) -> Option<String> {
        let proxy = configured_proxy(&self.config)?;
        let host_port = proxy
            .rsplit("://")
            .next()
            .unwrap_or(&proxy)
            .trim_end_matches('/')
            .to_string();
        let listening = host_port
            .parse()
            .ok()
            .and_then(|addr| {
                std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()
            })
            .is_some();
        Some(format!(
            "  configured proxy: {proxy}{}",
            if listening {
                ""
            } else {
                "  (nothing is listening there)"
            }
        ))
    }

    pub fn new(config: ProviderConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(30));
        // No total timeout here: a long generation is not a failure. The wait for the
        // *response* is bounded at the call site instead.

        // Direct by default. `no_proxy()` is not decoration: without it reqwest applies
        // the platform's proxy setting, which on Windows comes from the registry and is
        // invisible from here. A configured proxy is then added on top, deliberately and
        // explicitly, so what is used is exactly what is written down.
        builder = builder.no_proxy();
        if let Some(proxy) = configured_proxy(&config) {
            let url = if proxy.contains("://") {
                proxy.clone()
            } else {
                format!("http://{proxy}")
            };
            builder = builder.proxy(
                reqwest::Proxy::all(&url)
                    .with_context(|| format!("proxy '{proxy}' is not a usable URL"))?,
            );
        }

        let client = builder.build().context("cannot build HTTP client")?;
        Ok(Provider { config, client })
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Stream one completion, invoking `on_event` for every incremental update.
    pub async fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[(String, String, Value)],
        mut on_event: impl FnMut(Event),
    ) -> Result<()> {
        let tools_payload: Vec<Value> = tools
            .iter()
            .map(|(name, description, parameters)| {
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": parameters,
                    }
                })
            })
            .collect();

        let mut body = json!({
            "model": self.config.model,
            "messages": ensure_tool_calls_are_answered(messages),
            "stream": true,
            // Ask for a final usage frame; harmless for servers that ignore it.
            "stream_options": { "include_usage": true },
        });
        if !tools_payload.is_empty() {
            body["tools"] = json!(tools_payload);
        }

        let key = self.config.resolved_key();
        let mut req = self.client.post(self.config.endpoint()).json(&body);
        if !key.is_empty() {
            req = req.bearer_auth(&key);
        }

        // A dead network has to be *named*, and so does the proxy in front of it. The
        // reader's question is whether to wait, fix the cable, start the proxy, or clear
        // the proxy setting -- and reqwest's bare "error sending request" answers none
        // of them.
        let resp = match tokio::time::timeout(Duration::from_secs(180), req.send()).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                let endpoint = self.config.endpoint();
                return Err(anyhow!(
                    "no network -- the request to {endpoint} did not get out.{}{e:#}",
                    self.proxy_note()
                        .map(|n| format!("\n{n}\n  "))
                        .unwrap_or_default()
                ));
            }
            Err(_) => {
                return Err(anyhow!(
                    "no response from {} after 180s -- the connection was established but \
                     the server never answered. Usually the network dropped, or a proxy is \
                     swallowing the request.{}",
                    self.config.endpoint(),
                    self.proxy_note().map(|n| format!("\n{n}")).unwrap_or_default()
                ))
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "provider '{}' returned HTTP {}: {}",
                self.config.name,
                status,
                crate::util::truncate(text.trim(), 800)
            ));
        }

        let mut stream = resp.bytes_stream();
        let mut parser = StreamParser::default();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("stream error while reading response")?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Only consume whole lines; a partial frame stays in the buffer.
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                for event in parser.feed_line(&line) {
                    on_event(event);
                }
                if parser.done {
                    break;
                }
            }

            if parser.done {
                break;
            }
        }

        // Surface the fully assembled tool calls so the agent can execute them.
        for event in parser.finish() {
            on_event(event);
        }

        Ok(())
    }
}

/// Incremental parser for an OpenAI-compatible SSE stream.
///
/// Feed it one complete line at a time. It never blocks, never allocates a
/// runtime, and holds exactly the state needed to reassemble tool calls.
#[derive(Default)]
pub struct StreamParser {
    /// Tool calls under construction, keyed by the provider's `index`.
    calls: BTreeMap<u64, ToolCall>,
    /// Set when the `[DONE]` sentinel is seen.
    pub done: bool,
    /// Set when a `finish_reason` was observed.
    pub finish_reason: Option<String>,
}

impl StreamParser {
    /// Parse one line, returning any events it produced.
    ///
    /// Non-data lines, keep-alives and unparseable payloads are ignored rather
    /// than treated as errors: providers insert all three.
    pub fn feed_line(&mut self, line: &str) -> Vec<Event> {
        let mut events = Vec::new();

        let Some(data) = line.strip_prefix("data:") else {
            return events;
        };
        let data = data.trim();
        if data.is_empty() {
            return events;
        }
        if data == "[DONE]" {
            self.done = true;
            return events;
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                // A payload that *looks* like JSON but does not parse means the
                // provider changed something. Staying silent here is how you
                // get an agent that mysteriously does nothing, so say so.
                if data.starts_with('{') {
                    events.push(Event::Warning(format!(
                        "could not parse a stream frame from the provider: {e}. Raw: {}",
                        crate::util::truncate(data, 200)
                    )));
                }
                return events;
            }
        };

        // A usage-only frame is the final frame on OpenAI and DeepSeek.
        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            events.push(Event::Usage(Usage {
                prompt_tokens: usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                completion_tokens: usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            }));
        }

        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return events;
        };

        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if !reason.is_empty() {
                    self.finish_reason = Some(reason.to_string());
                }
            }

            let Some(delta) = choice.get("delta") else {
                continue;
            };

            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(Event::Text(text.to_string()));
                }
            }
            // DeepSeek-style reasoning channel.
            if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
                if !reasoning.is_empty() {
                    events.push(Event::Reasoning(reasoning.to_string()));
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let entry = self.calls.entry(index).or_default();

                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            entry.id = id.to_string();
                        }
                    }

                    if let Some(func) = tc.get("function") {
                        // The function name is the last thing we need before the
                        // call is identifiable; the `ToolStart` event is emitted
                        // from `finish()` so it can carry the provider's real id.
                        if let Some(name) = func.get("name").and_then(Value::as_str) {
                            if !name.is_empty() && entry.name.is_empty() {
                                entry.name = name.to_string();
                            }
                        }
                        // Arguments arrive in fragments: append, never replace.
                        if let Some(args) = func.get("arguments").and_then(Value::as_str) {
                            entry.arguments.push_str(args);
                        }
                    }
                }
            }
        }

        events
    }

    /// Completed tool calls. Call once the stream ends.
    ///
    /// Emits `ToolStart` and `ToolArgs` together, using the *same* id, so the
    /// consumer can always correlate them. The id is resolved here rather than
    /// when the function name arrives because the provider may not have sent
    /// its real id yet at that point — synthesising one early would produce two
    /// different ids for one call, and the arguments would be lost.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for (index, call) in std::mem::take(&mut self.calls) {
            if call.name.is_empty() {
                continue;
            }
            let id = if call.id.is_empty() {
                id_for(index)
            } else {
                call.id.clone()
            };
            let arguments = if call.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                call.arguments.clone()
            };
            events.push(Event::ToolStart {
                id: id.clone(),
                name: call.name.clone(),
            });
            events.push(Event::ToolArgs {
                id,
                args: arguments,
            });
        }
        events
    }
}

/// Providers are inconsistent about ids for streamed tool calls; synthesise a
/// stable one when absent so tool results can be correlated.
pub fn id_for(index: u64) -> String {
    format!("call_{index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The exact wire shape the API requires for an assistant turn that calls a
    /// tool. Getting this wrong is fatal but quiet: the first request succeeds,
    /// then every follow-up is rejected with a 400 because the echoed-back
    /// tool_calls are malformed.
    #[test]
    fn assistant_tool_calls_use_the_openai_wire_shape() {
        let msg = Message::Assistant {
            content: None,
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            }],
        };
        let v = serde_json::to_value(&msg).expect("must serialise");
        let tc = &v["tool_calls"][0];

        assert_eq!(tc["id"], "call_1");
        assert_eq!(
            tc["type"], "function",
            "the `type` discriminator is required, got {tc}"
        );
        assert_eq!(tc["function"]["name"], "bash");
        assert_eq!(tc["function"]["arguments"], r#"{"command":"ls"}"#);
        assert!(
            tc.get("name").is_none(),
            "name must not be emitted at the top level, got {tc}"
        );

        // A tool result must carry the id it answers.
        let tool_msg = Message::Tool {
            tool_call_id: "call_1".to_string(),
            content: "ok".to_string(),
        };
        let tv = serde_json::to_value(&tool_msg).expect("must serialise");
        assert_eq!(tv["role"], "tool");
        assert_eq!(tv["tool_call_id"], "call_1");
    }

    #[test]
    fn parses_plain_text_deltas() {
        let mut p = StreamParser::default();
        let mut all = Vec::new();
        for line in [
            r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
            "data: [DONE]",
        ] {
            all.extend(p.feed_line(line));
        }
        assert_eq!(texts(&all), "Hello");
        assert!(p.done);
    }

    #[test]
    fn ignores_comments_keepalives_and_garbage() {
        let mut p = StreamParser::default();
        for line in [
            ": keep-alive",
            "",
            "event: ping",
            "data: not-json",
            r#"data: {"choices":[]}"#,
        ] {
            assert!(
                p.feed_line(line).is_empty(),
                "line {line:?} should be inert"
            );
        }
        assert!(!p.done);
    }

    #[test]
    fn reassembles_tool_call_arguments_from_fragments() {
        let mut p = StreamParser::default();
        // This is the shape DeepSeek and OpenAI actually emit: the function
        // name arrives first, then the arguments in several pieces.
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"bash","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"comm"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls -la\"}"}}]}}]}"#,
            "data: [DONE]",
        ] {
            p.feed_line(line);
        }

        let finished = p.finish();
        let args = finished
            .iter()
            .find_map(|e| match e {
                Event::ToolArgs { args, .. } => Some(args.clone()),
                _ => None,
            })
            .expect("expected assembled tool args");
        assert_eq!(args, r#"{"command":"ls -la"}"#);
        // And it must be valid JSON by the time the agent sees it.
        let parsed: Value = serde_json::from_str(&args).unwrap();
        assert_eq!(parsed["command"], "ls -la");
    }

    /// Regression test for a bug that silently broke every tool call.
    ///
    /// `ToolStart` used to be emitted when the function name arrived, using an
    /// id synthesised from the stream index. `ToolArgs` was emitted at the end
    /// using the provider's real id. Those two ids differed, so a consumer
    /// correlating on id lost the arguments and invoked the tool with `{}`.
    #[test]
    fn tool_start_and_tool_args_share_the_same_id() {
        let mut p = StreamParser::default();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"bash","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
        ] {
            p.feed_line(line);
        }

        let mut started_id = None;
        let mut args_id = None;
        let mut name = None;
        for event in p.finish() {
            match event {
                Event::ToolStart { id, name: n } => {
                    started_id = Some(id);
                    name = Some(n);
                }
                Event::ToolArgs { id, .. } => args_id = Some(id),
                _ => {}
            }
        }

        assert_eq!(
            started_id, args_id,
            "ToolStart and ToolArgs must carry the same id or the call is lost"
        );
        assert_eq!(
            started_id.as_deref(),
            Some("call_abc"),
            "the provider's real id must win"
        );
        assert_eq!(name.as_deref(), Some("bash"));
    }

    /// The id synthesised on `ToolStart` must also match `finish()` when the
    /// provider never sends an id at all.
    #[test]
    fn synthesised_ids_are_consistent_between_start_and_args() {
        let mut p = StreamParser::default();
        p.feed_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":3,"function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        );
        let events = p.finish();
        let ids: Vec<String> = events
            .iter()
            .map(|e| match e {
                Event::ToolStart { id, .. } | Event::ToolArgs { id, .. } => id.clone(),
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["call_3", "call_3"]);
    }

    #[test]
    fn keeps_parallel_tool_calls_separate() {
        let mut p = StreamParser::default();
        let mut all = Vec::new();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"list","arguments":"{\"pa"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"x\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"th\":\".\"}"}}]}}]}"#,
        ] {
            all.extend(p.feed_line(line));
        }
        let finished = p.finish();
        let mut pairs: Vec<(String, String)> = finished
            .iter()
            .filter_map(|e| match e {
                Event::ToolArgs { id, args } => Some((id.clone(), args.clone())),
                _ => None,
            })
            .collect();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("a".to_string(), r#"{"path":"x"}"#.to_string()),
                ("b".to_string(), r#"{"path":"."}"#.to_string()),
            ]
        );
    }

    #[test]
    fn extracts_usage_and_reasoning() {
        let mut p = StreamParser::default();
        let mut all = Vec::new();
        all.extend(
            p.feed_line(r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#),
        );
        all.extend(p.feed_line(
            r#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7}}"#,
        ));

        assert!(matches!(all[0], Event::Reasoning(_)));
        match all[1] {
            Event::Usage(u) => {
                assert_eq!(u.prompt_tokens, 100);
                assert_eq!(u.completion_tokens, 7);
                assert_eq!(u.total(), 107);
            }
            ref other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn records_finish_reason() {
        let mut p = StreamParser::default();
        p.feed_line(r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
        assert_eq!(p.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn synthesises_id_when_provider_omits_one() {
        let mut p = StreamParser::default();
        p.feed_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"list","arguments":"{}"}}]}}]}"#,
        );
        let id = p
            .finish()
            .into_iter()
            .find_map(|e| match e {
                Event::ToolArgs { id, .. } => Some(id),
                _ => None,
            })
            .expect("expected ToolArgs");
        assert_eq!(id, "call_2");
    }

    #[test]
    fn empty_arguments_become_empty_object() {
        let mut p = StreamParser::default();
        p.feed_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"list"}}]}}]}"#,
        );
        let args = p
            .finish()
            .into_iter()
            .find_map(|e| match e {
                Event::ToolArgs { args, .. } => Some(args),
                _ => None,
            })
            .expect("expected ToolArgs");
        assert_eq!(args, "{}");
    }
}
