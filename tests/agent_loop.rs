//! End-to-end tests for the agent loop.
//!
//! A stub OpenAI-compatible server replays recorded SSE responses, so the whole
//! chain is exercised for real: request building -> SSE parsing -> tool
//! execution -> results fed back -> final answer.
//!
//! This is the test that proves flint works against a streaming provider
//! without needing an API key or a network.

use std::path::PathBuf;
use std::sync::Arc;

use flint::agent::Agent;
use flint::config::{Config, ProviderConfig};
use flint::event::Event;
use flint::provider::Provider;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Replays a fixed SSE body, ignoring the request contents.
struct SseFixture {
    body: String,
}

impl Respond for SseFixture {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(self.body.clone())
    }
}

fn sse(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\n\n");
    }
    out
}

fn test_config(base_url: &str) -> Config {
    Config {
        default_provider: "stub".to_string(),
        shell: if cfg!(windows) {
            "cmd".to_string()
        } else {
            "sh".to_string()
        },
        shell_args: if cfg!(windows) {
            vec!["/C".to_string()]
        } else {
            vec!["-c".to_string()]
        },
        max_tool_output: 10_000,
        max_steps: 10,
        readonly: false,
        proxy: None,
        verbose: false,
        tool_detail: false,
        providers: vec![ProviderConfig {
            name: "stub".to_string(),
            base_url: base_url.to_string(),
            api_key: "test".to_string(),
            model: "stub-model".to_string(),
            api_key_env: None,
        }],
    }
}

/// Frame sequence for a model that calls one tool then answers.
///
/// Note the escaping: these are raw string literals, so `\"` inside them is a
/// literal backslash-quote — which is exactly what the wire format carries,
/// since the tool arguments are a *string* containing JSON.
fn tool_then_answer() -> String {
    sse(&[
        // Turn 1: request a bash tool call, arguments split across frames.
        r#"data: {"choices":[{"delta":{"content":"Let me check. "}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"com"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"mand\":\"echo flint-e2e-ok\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":11}}"#,
        "data: [DONE]",
    ])
}

/// Frame sequence for a model that just answers.
fn answer_only() -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"content":"All done."}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
        "data: [DONE]",
    ])
}

async fn agent_for(server: &MockServer, cwd: PathBuf) -> Agent {
    let config = test_config(&server.uri());
    let provider = Provider::new(config.providers[0].clone()).unwrap();
    Agent::new(&config, provider, false, cwd, None)
}

/// Direct probe of the shell execution path, independent of the agent.
///
/// This is the test that caught `cmd.exe` being spawned without `/C`, which
/// made every command print a banner and exit 0 without running anything.
#[tokio::test]
async fn shell_execution_actually_runs_the_command() {
    let config = test_config("http://unused");
    let out =
        flint::tools::run_command_raw(&config, "echo flint-probe-ok", &std::env::temp_dir(), 30)
            .await
            .expect("run_command_raw should succeed");

    assert!(
        out.contains("flint-probe-ok"),
        "the shell must actually run the command. resolved shell = {:?}, output = {out:?}",
        flint::tools::probe_shell(&config.shell, &config.shell_args)
    );
    assert!(
        !out.contains("Microsoft Windows [Version"),
        "the shell went interactive instead of running the command: {out:?}"
    );
}

/// A failing command must report a non-zero exit code, not silence.
#[tokio::test]
async fn shell_execution_reports_failure() {
    let config = test_config("http://unused");
    let out = flint::tools::run_command_raw(&config, "exit 7", &std::env::temp_dir(), 30)
        .await
        .expect("run_command_raw should succeed");
    assert!(
        out.contains("exit code: 7"),
        "a non-zero exit must be visible, got {out:?}"
    );
}

/// The exit code must survive as a *value*, not just as text in the report.
///
/// `flint exec` is the mode scripts are meant to drive, and it used to return
/// `Ok(0)` unconditionally: every failing command looked like success to the
/// caller. Printing "[exit code: 7]" was not enough, so this asserts the code
/// itself.
#[tokio::test]
async fn run_command_detailed_surfaces_the_real_exit_code() {
    let config = test_config("http://unused");

    let failed = flint::tools::run_command_detailed(&config, "exit 7", &std::env::temp_dir(), 30)
        .await
        .expect("should run");
    assert_eq!(
        failed.code, 7,
        "the child's exit code must be carried out, got {} (report: {:?})",
        failed.code, failed.report
    );
    assert!(!failed.success(), "exit 7 must not count as success");

    let ok = flint::tools::run_command_detailed(&config, "exit 0", &std::env::temp_dir(), 30)
        .await
        .expect("should run");
    assert_eq!(ok.code, 0, "a clean command must report 0");
    assert!(ok.success());
}

/// A configured proxy must actually reach the child process.
///
/// flint exists to repair tooling, and repairs download things. On a network
/// where direct access is blocked but a local proxy works, a flint that does not
/// forward the proxy fails exactly when it is needed.
#[tokio::test]
async fn configured_proxy_is_exported_to_the_child() {
    let mut config = test_config("http://unused");

    // Syntax differs: cmd.exe expands %VAR%, POSIX shells expand $VAR. Getting
    // this wrong makes the test compare against a literal, and pass or fail for
    // the wrong reason.
    let echo_proxy = if cfg!(windows) {
        "echo proxy=%HTTPS_PROXY%"
    } else {
        "echo proxy=$HTTPS_PROXY"
    };

    let plain = flint::tools::run_command_detailed(&config, echo_proxy, &std::env::temp_dir(), 30)
        .await
        .expect("should run");
    assert!(
        !plain.report.contains("http://127.0.0.1:9"),
        "no proxy configured, so it must not be injected: {:?}",
        plain.report
    );

    config.proxy = Some("http://127.0.0.1:9".to_string());
    let proxied =
        flint::tools::run_command_detailed(&config, echo_proxy, &std::env::temp_dir(), 30)
            .await
            .expect("should run");
    assert!(
        proxied.report.contains("http://127.0.0.1:9"),
        "the configured proxy must reach the child process, got {:?}",
        proxied.report
    );
}

/// Guards the fixtures themselves.
///
/// The SSE frames in this file are raw string literals containing JSON that
/// contains JSON. It is very easy to get the escaping wrong in a way that
/// produces a silently unparseable stream, which then makes every other test in
/// this file fail for a misleading reason. This test fails loudly instead.
#[test]
fn fixtures_are_well_formed() {
    for (label, body) in [
        ("tool_then_answer", tool_then_answer()),
        ("answer_only", answer_only()),
    ] {
        let mut saw_data_frame = false;
        for line in body.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                saw_data_frame = true;
                serde_json::from_str::<serde_json::Value>(data).unwrap_or_else(|e| {
                    panic!("{label}: frame is not valid JSON: {e}\n  {data}");
                });
            }
        }
        assert!(saw_data_frame, "{label} produced no data frames");
    }

    // Every fixture must end with the sentinel, or the stream never terminates.
    for body in [tool_then_answer(), answer_only()] {
        assert!(
            body.contains("data: [DONE]"),
            "fixtures must terminate with [DONE]"
        );
    }
}

#[tokio::test]
async fn runs_a_tool_call_and_reports_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: tool_then_answer(),
        })
        // Turn 1 asks for the tool; turn 2 gets the final answer.
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // The follow-up request (after the tool result) gets a plain answer.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let cwd = std::env::temp_dir();
    let mut agent = agent_for(&server, cwd).await;

    let mut text = String::new();
    let mut tool_starts = Vec::new();
    let mut tool_results = Vec::new();
    let mut usage = None;

    agent
        .run("check something", |ev| match ev {
            Event::Text(t) => text.push_str(&t),
            Event::ToolStart { name, .. } => tool_starts.push(name),
            Event::ToolResult { output, ok, .. } => tool_results.push((output, ok)),
            Event::Usage(u) => usage = Some(u),
            _ => {}
        })
        .await
        .expect("agent run should succeed");

    assert_eq!(
        tool_starts,
        vec!["bash"],
        "the model's tool call must execute"
    );
    assert_eq!(tool_results.len(), 1, "exactly one tool result");
    let (output, ok) = &tool_results[0];
    assert!(
        output.contains("flint-e2e-ok"),
        "tool output should carry the command's stdout, got: {output}"
    );
    assert!(ok, "the command exits 0, so it must be reported as ok");

    assert!(text.contains("Let me check."));
    assert!(text.contains("All done."), "final answer must be streamed");
    assert_eq!(
        usage.map(|u| u.prompt_tokens),
        Some(7),
        "last usage frame wins"
    );
}

#[tokio::test]
async fn history_contains_the_tool_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: tool_then_answer(),
        })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    agent.run("check something", |_| {}).await.unwrap();

    let history = agent.history_mut();
    let rendered = serde_json::to_string(&*history).unwrap();

    // system prompt, user, assistant(tool_calls), tool result, assistant answer
    assert!(
        rendered.contains("check something"),
        "the user message must be recorded"
    );
    assert!(
        rendered.contains("tool_calls") || rendered.contains("call_1"),
        "the assistant tool call must be recorded in history"
    );
    assert!(
        rendered.contains("flint-e2e-ok"),
        "the tool result must be fed back into history"
    );
    assert!(
        rendered.contains(SYSTEM_PROMPT_SNIPPET),
        "the system prompt must be present"
    );
}

const SYSTEM_PROMPT_SNIPPET: &str = "minimal command-line coding and system-repair agent";

#[tokio::test]
async fn survives_a_tool_that_fails() {
    let server = MockServer::start().await;
    // Ask for a command that exits non-zero.
    let failing = sse(&[
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":\"exit 3\"}"}}]}}]}"#,
        "data: [DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture { body: failing })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let mut results = Vec::new();
    agent
        .run("fail on purpose", |ev| {
            if let Event::ToolResult { output, ok, .. } = ev {
                results.push((output, ok));
            }
        })
        .await
        .expect("a failing tool must not abort the turn");

    assert_eq!(results.len(), 1);
    assert!(!results[0].1, "non-zero exit must be reported as not ok");
    assert!(
        results[0].0.contains("exit code: 3"),
        "the exit code must be visible to the model, got: {}",
        results[0].0
    );
}

#[tokio::test]
async fn readonly_refuses_a_mutating_command() {
    let server = MockServer::start().await;
    let mutating = sse(&[
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":\"rm -rf /tmp/flint-should-not-exist\"}"}}]}}]}"#,
        "data: [DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture { body: mutating })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let config = test_config(&server.uri());
    let provider = Provider::new(config.providers[0].clone()).unwrap();
    let mut agent = Agent::new(&config, provider, true, std::env::temp_dir(), None);

    let mut blocked = false;
    agent
        .run("delete something", |ev| {
            if let Event::ToolResult { output, ok, .. } = ev {
                if !ok && output.contains("readonly") {
                    blocked = true;
                }
            }
        })
        .await
        .unwrap();

    assert!(
        blocked,
        "readonly must block the mutating command and say so"
    );
    assert!(
        !std::path::Path::new("/tmp/flint-should-not-exist").exists(),
        "the refused command must not have run"
    );
}

#[tokio::test]
async fn reports_a_clear_error_on_http_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let err = agent.run("hello", |_| {}).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("401"),
        "the HTTP status must surface to the user, got: {msg}"
    );
    assert!(
        msg.contains("bad key"),
        "the provider's error body must be included, got: {msg}"
    );
}

#[tokio::test]
async fn malformed_tool_arguments_are_reported_not_fatal() {
    let server = MockServer::start().await;
    let bad = sse(&[
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{not json"}}]}}]}"#,
        "data: [DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture { body: bad })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let mut warned = false;
    agent
        .run("do something", |ev| {
            if matches!(ev, Event::Warning(_)) {
                warned = true;
            }
        })
        .await
        .expect("malformed arguments must not abort the turn");

    assert!(warned, "the user must be warned about malformed arguments");
}

/// Interrupting mid-tool-loop must not leave the history unsendable.
///
/// Reported from a real session: the user typed while the model was running tools,
/// and every message after that failed with "An assistant message with 'tool_calls'
/// must be followed by tool messages responding to each 'tool_call_id'".
///
/// The turn future is *dropped* to interrupt, which cancels the tool loop where it
/// stands, so the assistant message can end up asking for tools that never answered.
/// Dropping the future also means the loop's own cleanup never runs, which is why the
/// repair has to happen when the *next* turn starts. The session must fix itself or it
/// is bricked, not merely wrong for one turn.
#[tokio::test]
async fn an_interrupted_tool_loop_leaves_the_session_usable() {
    use flint::event::{Message, ToolCall};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;

    // Exactly the state a dropped turn leaves behind: the model asked for two tools,
    // the first ran, and the interrupt landed before the second.
    agent.history_mut().push(Message::Assistant {
        content: None,
        reasoning: None,
        tool_calls: vec![
            ToolCall {
                id: "call_a".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"echo first"}"#.to_string(),
            },
            ToolCall {
                id: "call_b".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"echo second"}"#.to_string(),
            },
        ],
    });
    agent.history_mut().push(Message::Tool {
        tool_call_id: "call_a".to_string(),
        content: "first".to_string(),
    });

    // The next turn has to repair the history rather than be rejected.
    agent
        .run("never mind", |_| {})
        .await
        .expect("a turn after an interrupt must be able to run");

    // Walk the history the way the API would, and insist the shape is valid: after
    // every assistant message with tool calls, a tool message per call, in order.
    let history = agent.history_mut();
    let mut index = 0;
    let mut repaired = false;
    while index < history.len() {
        let Message::Assistant { tool_calls, .. } = &history[index] else {
            index += 1;
            continue;
        };
        let asked: Vec<&str> = tool_calls.iter().map(|c| c.id.as_str()).collect();
        if asked.is_empty() {
            index += 1;
            continue;
        }
        let mut answered: Vec<&str> = Vec::new();
        let mut next = index + 1;
        while let Some(Message::Tool { tool_call_id, content }) = history.get(next) {
            if content.contains("interrupted by the user") {
                repaired = true;
            }
            answered.push(tool_call_id.as_str());
            next += 1;
        }
        assert_eq!(
            answered, asked,
            "every tool call must be answered, in order, before the next request"
        );
        index = next;
    }
    assert!(
        repaired,
        "the second call should have been answered by a placeholder saying it never ran"
    );
}

#[tokio::test]
async fn empty_response_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture {
            body: sse(&["data: [DONE]"]),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let warnings = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = warnings.clone();
    agent
        .run("hello", move |ev| {
            if let Event::Warning(w) = ev {
                sink.lock().unwrap().push(w);
            }
        })
        .await
        .unwrap();

    let warnings = warnings.lock().unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("empty")),
        "an empty response should warn instead of silently doing nothing, got {warnings:?}"
    );
}
