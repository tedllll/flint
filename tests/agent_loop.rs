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
        instructions: "hint".to_string(),
        skill_dirs: Vec::new(),
        search: None,
        providers: vec![ProviderConfig {
            name: "stub".to_string(),
            start: None,
            stop: None,
            start_timeout_secs: 0,
            base_url: base_url.to_string(),
            api_key: "test".to_string(),
            model: "stub-model".to_string(),
            // A second model, so anything that lists or chooses models has something to
            // list. Served by the same stub, which ignores the model name.
            models: vec!["stub-model".to_string(), "stub-other".to_string()],
            api_key_env: None,
            // Never a proxy in tests: a stub server lives on localhost, and inheriting a
            // dead system proxy would make the suite fail for reasons that have nothing
            // to do with the code under test.
            proxy: None,
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

/// Frame sequence for a model that asks for the same call three times in one round.
fn same_call_three_times() -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":2,"id":"call_c","function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// Frame sequence for a tool call whose arguments never parse as JSON.
///
/// The inner string is `{"path": "C:\work"}` -- a Windows path written without escaping the
/// backslash, which is the mistake models actually make. The *outer* frame is still valid
/// JSON, so this reaches the agent as a real call with broken arguments.
fn call_with_broken_arguments() -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_bad","function":{"name":"read","arguments":"{\"path\": \"C:\\work\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// Frame sequence for a model that just answers.
fn answer_only() -> String {    sse(&[
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

/// Frame sequence for a model that loads a skill, then answers.
fn skill_then_answer() -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_s","function":{"name":"skill","arguments":"{\"name\":\"tidy-commits\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// A temporary project with an instruction file and one skill in it.
fn skill_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-ctx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).expect("project dir");
    std::fs::create_dir_all(dir.join(".flint").join("skills").join("tidy-commits"))
        .expect("skills dir");
    std::fs::write(
        dir.join("AGENTS.md"),
        "Run cargo test before claiming success.\n",
    )
    .expect("agents file");
    std::fs::write(
        dir.join(".flint").join("skills").join("tidy-commits").join("SKILL.md"),
        "---\nname: tidy-commits\ndescription: Squash and reword the commits.\n---\n\nStep one: squash the fixups.\n",
    )
    .expect("skill file");
    dir
}

/// A tool call whose arguments are not JSON is reported to the user, to the record and to
/// the model -- and the turn goes on rather than stopping there.
///
/// The failure mode this guards against is a *silent* one: the call is announced, the model
/// is told what was wrong, and no result event is ever emitted. In a transcript that reads
/// as a call that is still running; in a `--json` stream it is a `tool.started` with no
/// `tool.completed`, which a caller waiting for the pair never recovers from.
#[tokio::test]
async fn a_call_with_unparseable_arguments_still_reports_a_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: call_with_broken_arguments(),
        })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let mut results: Vec<(String, bool)> = Vec::new();
    let mut warnings = 0;
    agent
        .run("read something", |ev| match ev {
            Event::ToolResult { output, ok, .. } => results.push((output, ok)),
            Event::Warning(_) => warnings += 1,
            _ => {}
        })
        .await
        .expect("the run itself must finish, not fail");

    assert_eq!(results.len(), 1, "the broken call produced no result");
    let (output, ok) = &results[0];
    assert!(!ok, "a call that never ran reported success: {output}");
    assert!(
        output.contains("invalid JSON arguments"),
        "the result does not say what was wrong: {output}"
    );
    assert_eq!(warnings, 1, "the user was not warned exactly once");

    // And the model is told, so it can correct itself on the next round.
    let requests = server.received_requests().await.expect("requests");
    let follow_up = String::from_utf8_lossy(&requests[1].body).to_string();
    assert!(
        follow_up.contains("invalid JSON arguments"),
        "the model never learned what was wrong: {follow_up}"
    );
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

/// A command that outlives its budget must be **killed**, not merely abandoned.
///
/// The old code wrapped `wait_with_output` in a timeout and returned an error, which
/// drops the future and leaves the child running. A timed-out `cargo build` therefore
/// kept building forever while flint calmly reported a timeout -- which is what makes a
/// machine feel wedged. The check is the only honest one: the command writes a file
/// *after* the timeout should have killed it, and the file must never appear.
#[tokio::test]
async fn a_timed_out_command_is_killed_not_abandoned() {
    let config = test_config("http://unused");
    let dir = std::env::temp_dir().join(format!("flint-timeout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("survived.txt");
    let _ = std::fs::remove_file(&marker);

    // Sleeps past the 2s budget, then writes the marker. If the process survives the
    // timeout, the marker shows up a few seconds later.
    let command = if cfg!(windows) {
        format!(
            "ping -n 5 127.0.0.1 > nul & echo survived > \"{}\"",
            marker.display()
        )
    } else {
        format!("sleep 4; echo survived > \"{}\"", marker.display())
    };

    let started = std::time::Instant::now();
    let result = flint::tools::run_command_raw(&config, &command, &std::env::temp_dir(), 2).await;
    let elapsed = started.elapsed();
    assert!(result.is_err(), "a command over its budget must report an error");
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("killed after"),
        "the error should say it was killed, got: {message}"
    );
    // It used to add "or write [timeout:N] before the command", and nothing in flint has
    // ever parsed that. A message that names a syntax the tool does not implement sends
    // the model to write it, watch the same timeout happen, and conclude the tool lies.
    assert!(
        !message.contains("[timeout"),
        "the error must not promise a marker nothing parses, got: {message}"
    );
    assert!(
        message.contains("timeout_secs"),
        "the error should name the argument that actually raises the budget, got: {message}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "it returned before the command would have finished on its own: {elapsed:?}"
    );

    // Well past when the orphan would have written the marker.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert!(
        !marker.exists(),
        "the command outlived its timeout and wrote {} -- it was abandoned, not killed",
        marker.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
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
        ("same_call_three_times", same_call_three_times()),
        ("call_with_broken_arguments", call_with_broken_arguments()),
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

/// The prompt must carry what the project wrote down.
///
/// Asserted on the request the stub actually received, not on a helper's return value.
/// The failure this catches is a note that is built correctly and never sent -- which is
/// how a feature like this reaches a user as "flint ignored my AGENTS.md".
#[tokio::test]
async fn the_prompt_names_project_instructions_and_the_skill_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let project = skill_workspace("prompt");
    let mut agent = agent_for(&server, project.clone()).await;
    agent.run("say ok", |_| {}).await.expect("run");

    let requests = server
        .received_requests()
        .await
        .expect("the stub records what it was sent");
    assert_eq!(requests.len(), 1, "one model call expected");
    let body = String::from_utf8_lossy(&requests[0].body).to_string();

    assert!(
        body.contains("AGENTS.md"),
        "the instruction file is not named in the prompt: {body}"
    );
    assert!(
        !body.contains("Run cargo test before claiming success."),
        "hint mode must name the file, not paste it: {body}"
    );
    assert!(
        body.contains("tidy-commits: Squash and reword the commits."),
        "the skill catalog is missing from the prompt: {body}"
    );
    assert!(
        body.contains("until it has been loaded"),
        "the catalog must not be mistaken for the instructions: {body}"
    );
    assert!(
        !body.contains("Step one: squash the fixups."),
        "the skill body must not be in the prompt before it is loaded: {body}"
    );

    let _ = std::fs::remove_dir_all(&project);
}

/// The catalog is useless if loading what it names does not work.
///
/// Both halves are asserted: the tool result the model gets, and the request that follows
/// it. A body returned to the loop but never sent back to the model would look identical
/// from the outside.
#[tokio::test]
async fn the_skill_tool_returns_a_body_that_reaches_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: skill_then_answer(),
        })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let project = skill_workspace("skill-tool");
    let mut agent = agent_for(&server, project.clone()).await;

    let mut results: Vec<String> = Vec::new();
    agent
        .run("tidy the commits", |ev| {
            if let Event::ToolResult { output, .. } = ev {
                results.push(output);
            }
        })
        .await
        .expect("run");

    assert_eq!(results.len(), 1, "the skill call must run once");
    assert!(
        results[0].contains("Step one: squash the fixups."),
        "the tool did not return the body: {}",
        results[0]
    );

    let requests = server.received_requests().await.expect("requests");
    let follow_up = String::from_utf8_lossy(&requests[1].body).to_string();
    assert!(
        follow_up.contains("Step one: squash the fixups."),
        "the loaded body never reached the model: {follow_up}"
    );

    let _ = std::fs::remove_dir_all(&project);
}

/// `exec` reaches the model, and its schema asks for an array of arguments.
///
/// Asserted on the request body rather than on the toolbox, because a tool the model is
/// never told about is a tool that does not exist -- and because the *shape* of `args` in
/// the schema is what decides whether the model sends a list or a command line. A schema
/// that said "array" while the tool expected a string would fail only in production.
#[tokio::test]
async fn exec_is_offered_to_the_model_with_an_argument_array() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    assert!(
        agent.tool_names().iter().any(|n| n == "exec"),
        "exec is not registered: {:?}",
        agent.tool_names()
    );
    agent.run("say something", |_| {}).await.expect("run");

    let requests = server.received_requests().await.expect("requests");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("the request body is JSON");
    let exec = body["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["function"]["name"] == "exec")
        .expect("exec is not in the tools sent to the model");

    let params = &exec["function"]["parameters"]["properties"];
    assert_eq!(
        params["args"]["type"], "array",
        "the model must be told to send a list: {params}"
    );
    assert_eq!(
        params["args"]["items"]["type"], "string",
        "each element is one argument: {params}"
    );
    assert_eq!(
        exec["function"]["parameters"]["required"][0], "program",
        "program is the one thing that must be there"
    );
    assert!(
        params["stdin"]["type"] == "string",
        "a payload that is not an argument still needs somewhere to go: {params}"
    );
}

/// A project with no skills must not pay for a tool that can only say "none".
#[tokio::test]
async fn the_skill_tool_is_absent_when_there_are_no_skills() {
    let server = MockServer::start().await;
    let empty = std::env::temp_dir().join(format!("flint-no-skills-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).expect("temp dir");

    let agent = agent_for(&server, empty.clone()).await;
    assert!(
        !agent.tool_names().iter().any(|n| n == "skill"),
        "the skill tool is offered with nothing to load: {:?}",
        agent.tool_names()
    );

    let _ = std::fs::remove_dir_all(&empty);
}

/// The third identical call says so, and says it to the model.
///
/// Asserted on the request that follows, not only on the events: a note that reaches the
/// transcript and not the model is a note to the user, and the model is the one that has
/// to stop. Nothing is blocked -- a repeat is sometimes right -- so the check is that the
/// call still ran and the note arrived with it.
#[tokio::test]
async fn a_repeated_call_is_pointed_out_to_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: same_call_three_times(),
        })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let mut outputs: Vec<String> = Vec::new();
    agent
        .run("list the directory three times", |ev| {
            if let Event::ToolResult { output, .. } = ev {
                outputs.push(output);
            }
        })
        .await
        .expect("run");

    assert_eq!(outputs.len(), 3, "all three calls must still run");
    assert!(
        !outputs[0].contains("identical call") && !outputs[1].contains("identical call"),
        "a note on a first or second call: {outputs:?}"
    );
    assert!(
        outputs[2].contains("3rd identical call"),
        "the third identical call went unremarked: {}",
        outputs[2]
    );

    let requests = server.received_requests().await.expect("requests");
    let follow_up = String::from_utf8_lossy(&requests[1].body).to_string();
    assert!(
        follow_up.contains("3rd identical call"),
        "the note never reached the model: {follow_up}"
    );
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

/// A phrase from the system prompt, used to prove it reached the history.
///
/// Deliberately a *behavioural* line rather than the opening description. The opening
/// was `minimal command-line coding and system-repair agent`, and when the prompt was
/// rewritten to stop framing every request as a repair, this test failed for a reason
/// that had nothing to do with what it checks -- that the system message is in the
/// history at all. Pinning a rule instead keeps the test about the plumbing.
const SYSTEM_PROMPT_SNIPPET: &str = "Act, do not narrate.";

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

// --- retrying a transient failure ------------------------------------------------

/// A 503 is the provider being briefly broken, so the turn must survive it.
///
/// The failure this guards against is the opposite of a crash: the request goes out, the
/// server answers "not right now", and the whole turn is reported as failed even though
/// the identical request would have succeeded a second later.
#[tokio::test]
async fn a_transient_status_is_retried_and_the_turn_succeeds() {
    let server = MockServer::start().await;

    // Priority 1 so this is consulted first, and `up_to_n_times(1)` so it stops matching
    // once it has fired; the second attempt then falls through to the fixture below.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream is restarting"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SseFixture { body: answer_only() })
        .with_priority(2)
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let mut seen: Vec<String> = Vec::new();
    agent
        .run("hello", |event| {
            if let Event::Text(t) = event {
                seen.push(t);
            }
        })
        .await
        .expect("a 503 must not fail the turn");

    // The retry must not double up. A failed attempt's events are buffered and dropped,
    // so only the attempt that completed contributes -- and because the agent assembles
    // the answer from accumulated deltas, seeing the text twice here would produce a
    // doubled sentence on screen.
    let joined = seen.join("");
    assert!(!joined.is_empty(), "no text reached the callback at all");
    assert!(
        joined.matches("你好").count() <= 1,
        "the answer was delivered more than once, so a failed attempt leaked its events: {joined:?}"
    );
}

/// A 401 will fail identically every time, so retrying only delays the bad news.
#[tokio::test]
async fn a_permanent_status_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let started = std::time::Instant::now();
    let err = agent.run("hello", |_| {}).await.unwrap_err();
    let msg = format!("{err:#}");

    assert!(msg.contains("401"), "got: {msg}");
    // The retry ladder starts at one second and doubles, so a single wasted retry would
    // already show up well past this.
    assert!(
        started.elapsed() < std::time::Duration::from_millis(900),
        "a 401 was retried; it took {:?}",
        started.elapsed()
    );
    assert!(
        !msg.contains("gave up after"),
        "a permanent failure must not report the retry ladder: {msg}"
    );
}

/// The backoff ladder itself: bounded, and never zero.
#[test]
fn retry_delays_grow_and_stay_bounded() {
    use flint::provider::retry_delay_for_test as retry_delay;
    let delays: Vec<u64> = (1..=8).map(|n| retry_delay(n).as_secs()).collect();
    assert!(delays.iter().all(|&d| d >= 1), "a zero wait would hammer: {delays:?}");
    assert!(
        delays.windows(2).all(|w| w[1] >= w[0]),
        "the ladder must not shrink: {delays:?}"
    );
    assert!(
        delays.iter().all(|&d| d <= 16),
        "the ladder must stay bounded: {delays:?}"
    );
}

/// What `debug prompt-input` prints is what the next request actually sends.
///
/// This is the only assertion that makes the command worth having. A preview that is
/// *described* as faithful drifts the moment someone edits the real path, and it drifts in
/// the direction nobody notices: it stays right about the obvious fields and goes wrong
/// about the interesting ones -- the pruning, the tool schemas, the repair of a dangling
/// tool call. So the comparison is against the bytes the stub server received, for the same
/// message, from the same code.
#[tokio::test]
async fn the_prompt_preview_is_the_request_that_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let preview = agent.request_preview(Some("say something"));
    agent.run("say something", |_| {}).await.expect("run");

    let requests = server.received_requests().await.expect("requests");
    let sent: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("the request body is JSON");
    assert_eq!(
        preview, sent,
        "the preview and the request have drifted apart, which is the one failure this \
         command must not have"
    );
}

/// A preview with no message shows the conversation as it stands, and sends nothing.
///
/// A diagnostic that quietly made a request would be worse than no diagnostic: the run it
/// describes would be a run it caused. The stub server is mounted with an expectation of
/// zero calls for the same reason a request would be invisible otherwise -- an empty mock
/// and an unreachable endpoint look identical.
#[tokio::test]
async fn a_preview_asks_for_no_message_and_sends_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .expect(0)
        .mount(&server)
        .await;

    let agent = agent_for(&server, std::env::temp_dir()).await;
    let preview = agent.request_preview(None);

    let messages = preview["messages"].as_array().expect("messages");
    assert_eq!(
        messages.len(),
        1,
        "an untouched conversation is the system prompt and nothing else: {messages:?}"
    );
    assert_eq!(messages[0]["role"], "system");
    assert!(
        !preview["tools"].as_array().expect("tools").is_empty(),
        "the tool schemas are part of what the model is sent, so they belong in the preview"
    );
}
