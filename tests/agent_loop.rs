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

/// What a command run by these tests is told about the run it is in -- nothing.
///
/// These call the shell runner directly, outside any `Agent`, to measure the runner itself (the
/// proxy variables, the timeout, the quoting). A run that has no conversation and no endpoint names
/// neither, which is the same answer `flint exec` gives.
fn bare() -> flint::tools::RunEnv {
    flint::tools::RunEnv::default()
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
        // Off in these tests: they are about one turn, and a budget that dropped a turn would be a
        // second thing changing under the assertion.
        max_request_chars: 0,
        max_steps: 10,
        readonly: false,
        proxy: None,
        verbose: flint::display::Verbosity::On,
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
            // This endpoint *can* be asked for reasoning, so a test that sets a level exercises
            // the sending rather than the "no field for this provider" path.
            thinking_field: "reasoning_effort".to_string(),
        }],
        thinking: "off".to_string(),
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
/// Frame sequence for a model that asks for two commands in one message.
///
/// The first command waits for a file only the second one writes, so the two can both succeed only
/// if they were in flight together. That is the evidence the test below asserts on: not how long the
/// turn took -- a clock would be a guess about the machine this runs on -- but whether one call ever
/// saw the other.
///
/// `ping` is the portable sleep for `cmd`: it is on every Windows, and `timeout` refuses to run when
/// stdin is not a terminal, which is exactly how a tool call runs a command. The arguments are built
/// through `serde_json` rather than written by hand because this is JSON inside a JSON string inside
/// a data frame, and the escaping is the part nobody should be reading in a test fixture.
fn two_commands_in_one_message() -> String {
    let (wait, touch) = if cfg!(windows) {
        (
            // `cmd`'s `if` swallows the rest of the line, `&` and all, when its condition is false --
            // so the success message is the *then* branch here rather than a command after the `if`.
            // Measured, not guessed: with the marker present the first spelling printed nothing.
            r#"ping -n 3 127.0.0.1 >nul & if exist b.txt (echo saw-b) else (exit /b 9)"#,
            "echo b > b.txt",
        )
    } else {
        (
            r#"sleep 2; [ -f b.txt ] || exit 9; echo saw-b"#,
            "echo b > b.txt",
        )
    };

    let frame = |index: usize, id: &str, command: &str| {
        let arguments = serde_json::to_string(
            &serde_json::json!({ "command": command }).to_string(),
        )
        .expect("a quoted argument string");
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":\"{id}\",\
             \"function\":{{\"name\":\"bash\",\"arguments\":{arguments}}}}}]}}}}]}}"
        )
    };
    let first = frame(0, "call_1", wait);
    let second = frame(1, "call_2", touch);
    sse(&[
        first.as_str(),
        second.as_str(),
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// Two calls of one message run at once.
///
/// This is the whole of what item 12 from the reading of Pi bought, and the proof is a rendezvous
/// rather than a stopwatch: run one after the other, the first call *cannot* pass, because the file
/// it waits for is written by a call that has not started yet. Nothing here depends on how fast the
/// machine is, and the failure a regression would produce is the tool's own exit code rather than a
/// timing that came out slightly too large.
///
/// The order of the results is asserted too, because it is part of the promise: the work is
/// concurrent and the *report* is in the order the model asked, so a transcript reads the same
/// whether the calls ran together or one at a time.
#[tokio::test]
async fn the_calls_of_one_message_run_at_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: two_commands_in_one_message(),
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

    let dir = std::env::temp_dir().join(format!("flint-parallel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let mut agent = agent_for(&server, dir.clone()).await;
    let mut results: Vec<String> = Vec::new();
    agent
        .run("run both of these", |ev| {
            if let Event::ToolResult { output, .. } = ev {
                results.push(output);
            }
        })
        .await
        .expect("the turn itself must finish, not fail");

    assert_eq!(
        results.len(),
        2,
        "one of the two calls produced no result at all: {results:?}"
    );
    assert!(
        !results[0].contains("[exit code:"),
        "the first call finished before the second one had started, so the calls of one message ran \
         one at a time: {}",
        results[0]
    );
    assert!(
        results[0].contains("saw-b"),
        "the first call did not report seeing the marker: {}",
        results[0]
    );
    assert!(
        dir.join("b.txt").exists(),
        "the second call never wrote its marker"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

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
        flint::tools::run_command_raw(&config, &bare(), "echo flint-probe-ok", &std::env::temp_dir(), 30)
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

/// A quoted command has to reach the shell exactly as the model wrote it.
///
/// `cmd.exe` takes a command line, not an argument list, and its parsing is not the C
/// runtime's; std quotes arguments the C runtime's way. So on Windows `echo "hello"` used to
/// come back as `\"hello\"`, and a quoted path -- an everyday thing to write -- failed with
/// "The filename, directory name, or volume label syntax is incorrect", which reads like the
/// path is wrong. Both cases are asserted here, because the first is the character-level
/// damage and the second is what it costs: a command that cannot be run at all.
#[cfg(windows)]
#[tokio::test]
async fn a_quoted_command_reaches_the_shell_verbatim() {
    let config = test_config("http://unused");

    let out = flint::tools::run_command_raw(&config, &bare(), "echo \"hello world\"", &std::env::temp_dir(), 30)
        .await
        .expect("the command should run");
    assert!(
        out.contains("\"hello world\""),
        "the quotes must arrive as quotes: {out:?}"
    );
    assert!(
        !out.contains('\\'),
        "a backslash was inserted in front of a quote: {out:?}"
    );

    // The system directory is one whose path nobody controls, and it exists on every Windows.
    let out = flint::tools::run_command_raw(
        &config,
        &bare(),
        "dir /b \"C:\\Windows\\System32\\drivers\\etc\"",
        &std::env::temp_dir(),
        30,
    )
    .await
    .expect("the command should run");
    assert!(
        out.contains("hosts"),
        "a quoted path must be usable, got: {out:?}"
    );

    // And a redirect into a quoted path, which is how a script writes a file.
    let dir = std::env::temp_dir().join(format!("flint-quoted-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("written by a command.txt");
    let command = format!(
        "echo one > \"{}\" && type \"{}\"",
        file.display(),
        file.display()
    );
    let out = flint::tools::run_command_raw(&config, &bare(), &command, &dir, 30)
        .await
        .expect("the command should run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.contains("one"),
        "a quoted redirect target must work, got: {out:?}"
    );
}

/// The `pwsh` tool: the script a model wrote is what runs, and what it wrote survives.
///
/// Every part of this is a measured requirement rather than a preference. The script's own
/// non-ASCII text is the byte-order-mark check: without the mark, `你好` in a script file was
/// measured coming back as `浣犲ソ` with exit code 0, which is the worst kind of wrong --
/// silent. `$args` is checked because an argument list is where a second parser usually gets
/// its chance to lose something. The path and the version are checked because the result has
/// to be enough to debug what ran: PowerShell 5.1 and 7 do not parse the same language.
#[cfg(windows)]
#[tokio::test]
async fn a_powershell_script_runs_from_the_file_it_was_written_to() {
    let dir = std::env::temp_dir().join(format!("flint-pwsh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let spill = dir.join("spill");

    let config = test_config("http://unused");
    let tools =
        flint::tools::ToolBox::new(&config, false, dir.clone()).with_spill_dir(spill.clone());
    let out = tools
        .invoke(
            "pwsh",
            &serde_json::json!({
                "script": "$name = \"你好\"\nWrite-Output \"$name $($args -join '|') $((1 + 1))\"\n",
                "args": ["one", "two words"],
            }),
        )
        .await
        .expect("the script should run");

    // The script's own text survived the trip: this is the byte-order mark.
    assert!(
        out.contains("你好"),
        "the script's non-ASCII text was misread: {out}"
    );
    assert!(
        !out.contains('\u{fffd}') && !out.contains("浣犲ソ"),
        "the script was read as the machine's code page: {out}"
    );
    // Multi-line, quoting, arguments and arithmetic all behaved as they do in a file.
    assert!(
        out.contains("one|two words 2"),
        "the script did not run as written: {out}"
    );
    // And the result says which PowerShell ran it, and where the script now is.
    assert!(
        out.contains("script-1.ps1"),
        "the result must name the script: {out}"
    );
    assert!(
        out.contains("(powershell)") || out.contains("(pwsh)"),
        "the result must name the PowerShell: {out}"
    );

    // The script is on disk, byte for byte, behind a BOM -- the artifact the tool exists for.
    let script = std::fs::read(spill.join("script-1.ps1")).expect("the script must be on disk");
    assert_eq!(
        &script[..3],
        &[0xef, 0xbb, 0xbf],
        "a script without a byte-order mark is read as the machine's code page"
    );
    assert!(
        std::str::from_utf8(&script[3..]).unwrap().contains("你好"),
        "the script on disk is not the script that was asked for"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A script is arbitrary code, and flint says so rather than guessing.
///
/// `is_readonly_command` reads a cmd or POSIX command line; a PowerShell script is a different
/// language, and answering about it in the wrong vocabulary would be a refusal that is wrong
/// in both directions -- `Get-Process | Stop-Process` is not obviously a mutation to a
/// classifier looking for `rm`.
#[cfg(windows)]
#[tokio::test]
async fn pwsh_refuses_a_script_in_readonly_mode() {
    let dir = std::env::temp_dir().join(format!("flint-pwsh-ro-{}", std::process::id()));
    let config = test_config("http://unused");
    let tools = flint::tools::ToolBox::new(&config, true, dir.clone());

    let error = tools
        .invoke("pwsh", &serde_json::json!({ "script": "Get-Date" }))
        .await
        .expect_err("readonly must refuse a script");
    assert!(
        error.to_string().contains("readonly"),
        "the refusal must say why: {error}"
    );
}

/// A Windows console program writes the machine's code page, and that is not UTF-8.
///
/// On a machine with a Chinese locale the page is 936, so every non-ASCII character in a
/// child's output used to arrive at the model as U+FFFD: it was shown an unreadable error
/// message and told nothing about why. The bytes are put in a file and handed over with
/// `type`, which copies them through untouched, so what is being asserted is flint's decoding
/// and not which code page some child happened to choose -- `chcp` is console-wide mutable
/// state, and while measuring this the same terminal was seen at 936 and at 65001 depending on
/// what had run in it before.
///
/// Skipped where the locale is not 936, rather than asserted around: the conversion itself has
/// a unit test in `util` that names its code page explicitly and runs anywhere.
#[cfg(windows)]
#[tokio::test]
async fn code_page_output_reaches_the_model_as_text() {
    if flint::util::ansi_code_page() != 936 {
        eprintln!("skipped: this machine's ANSI code page is not 936");
        return;
    }
    let config = test_config("http://unused");
    let dir = std::env::temp_dir().join(format!("flint-cp936-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // "你好.txt" as a CP936 console program writes it.
    let gbk = [0xc4u8, 0xe3, 0xba, 0xc3, b'.', b't', b'x', b't'];
    let file = dir.join("chars.txt");
    std::fs::write(&file, gbk).expect("the fixture");
    let command = format!("type {}", file.display());

    let out = flint::tools::run_command_raw(&config, &bare(), &command, &dir, 30)
        .await
        .expect("the command should run");

    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !out.contains('\u{fffd}'),
        "the code page bytes were replaced instead of decoded: {out:?}"
    );
    assert!(
        out.contains("你好.txt"),
        "the tool result must carry the text the command wrote, got: {out:?}"
    );
}

/// A killed command must take its own children with it.
///
/// The test above passes on Windows without any tree kill, and that is why this one exists:
/// its work was the second half of the same `cmd` line, so killing the shell was enough. A
/// command that *starts* something is the case that shows the difference -- `cmd.exe` has no
/// exec, so it is the parent of whatever it runs, and `kill_on_drop` ends the shell and
/// nothing else. Measured before the fix: both the child `cmd` and the `ping` under it were
/// still running after the shell was killed, and the marker appeared.
///
/// The child here is a real second process that outlives its parent unless the tree is ended.
/// The control run first is what makes the killed run mean something: without it, a quoting
/// mistake would look exactly like a successful kill.
#[cfg(windows)]
#[tokio::test]
async fn a_killed_command_takes_its_children_with_it() {
    let config = test_config("http://unused");
    let dir = std::env::temp_dir().join(format!("flint-tree-kill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("child-survived.txt");
    let child = dir.join("child.cmd");
    // A batch file rather than one long quoted command line: the shell reads it from a file,
    // so nothing in this test depends on how a nested quote survives `/C`. `ping` is the
    // sleep Windows has without a shell builtin to run it.
    std::fs::write(
        &child,
        format!(
            "@echo off\r\nping -n 4 127.0.0.1 > nul\r\necho alive > \"{}\"\r\n",
            marker.display()
        ),
    )
    .expect("the child script");
    let command = format!("cmd /C {}", child.display());

    // The control: uninterrupted, this command leaves the marker, so the assertion below is
    // about the kill and not about the command never having run.
    let _ = std::fs::remove_file(&marker);
    let out = flint::tools::run_command_raw(&config, &bare(), &command, &dir, 30)
        .await
        .expect("the control run should finish");
    assert!(
        marker.exists(),
        "the control did not produce the marker, so this test proves nothing: {out:?}"
    );

    // Now under a budget that runs out while the child is still sleeping.
    let _ = std::fs::remove_file(&marker);
    let result = flint::tools::run_command_raw(&config, &bare(), &command, &dir, 1).await;
    assert!(result.is_err(), "a command over its budget must report an error");

    // Well past when the child would have written the marker on its own.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert!(
        !marker.exists(),
        "the command's child outlived the kill and wrote {} -- the shell was killed, not the tree",
        marker.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same fact as the test above, on the other platform -- and it is the half that was a
/// documented gap for a while, because Windows is where the damage was measured: `taskkill /T` was
/// built for it and Unix was left with "`sh -c` usually *becomes* the command, so killing it is
/// usually enough".
///
/// Usually is the whole problem, and this is the case where it is not true. The shell here starts a
/// second process and waits for it, which is what a real command that backgrounds work looks like;
/// killing the shell alone leaves that process to write the marker at its leisure. On Unix there
/// was no process group in play for anything to signal, so there was nothing to kill *but* the
/// shell. `KillTree::detach` is the fix and this is what says so: the group dies with the command,
/// and the marker never appears.
///
/// The control run is here for the same reason it is in the test above: a quoting mistake in this
/// script would look exactly like a successful kill.
#[cfg(unix)]
#[tokio::test]
async fn a_killed_command_takes_its_children_with_it_on_unix() {
    let config = test_config("http://unused");
    let dir = std::env::temp_dir().join(format!("flint-tree-kill-unix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("child-survived.txt");
    let _ = std::fs::remove_file(&marker);

    // A subshell in the background, deliberately: `sleep 2; echo …` in a *list* is a process the
    // shell has to fork, and `wait` is what keeps the shell itself alive -- so the process that
    // writes the marker outlives its parent unless the whole group is signalled. A single simple
    // command would be exec'd into and would prove nothing about children.
    let command = format!("(sleep 2; echo alive > '{}') & wait", marker.display());

    // The control: uninterrupted, this command leaves the marker, so the assertion below is about
    // the kill and not about the command never having run.
    let out = flint::tools::run_command_raw(&config, &bare(), &command, &dir, 30)
        .await
        .expect("the control run should finish");
    assert!(
        marker.exists(),
        "the control did not produce the marker, so this test proves nothing: {out:?}"
    );

    // Now under a budget that runs out while the subshell is still sleeping.
    let _ = std::fs::remove_file(&marker);
    let result = flint::tools::run_command_raw(&config, &bare(), &command, &dir, 1).await;
    assert!(result.is_err(), "a command over its budget must report an error");

    // Well past when the subshell would have written the marker on its own.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert!(
        !marker.exists(),
        "the command's child outlived the kill and wrote {} -- the shell was killed, not the \
         process group",
        marker.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
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
    let result = flint::tools::run_command_raw(&config, &bare(), &command, &std::env::temp_dir(), 2).await;
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
    let out = flint::tools::run_command_raw(&config, &bare(), "exit 7", &std::env::temp_dir(), 30)
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

    let failed = flint::tools::run_command_detailed(&config, &bare(), "exit 7", &std::env::temp_dir(), 30)
        .await
        .expect("should run");
    assert_eq!(
        failed.code, 7,
        "the child's exit code must be carried out, got {} (report: {:?})",
        failed.code, failed.report
    );
    assert!(!failed.success(), "exit 7 must not count as success");

    let ok = flint::tools::run_command_detailed(&config, &bare(), "exit 0", &std::env::temp_dir(), 30)
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

    let plain = flint::tools::run_command_detailed(&config, &bare(), echo_proxy, &std::env::temp_dir(), 30)
        .await
        .expect("should run");
    assert!(
        !plain.report.contains("http://127.0.0.1:9"),
        "no proxy configured, so it must not be injected: {:?}",
        plain.report
    );

    config.proxy = Some("http://127.0.0.1:9".to_string());
    let proxied =
        flint::tools::run_command_detailed(&config, &bare(), echo_proxy, &std::env::temp_dir(), 30)
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

    // The retry must not double up. This 503 never reached the body, so nothing was drawn
    // and the retry is free -- which is the rule now: the ladder stops at the first drawn
    // character, and everything before it is still retried and still leaves no trace.
    let joined = seen.join("");
    assert!(!joined.is_empty(), "no text reached the callback at all");
    assert!(
        joined.matches("你好").count() <= 1,
        "the answer was delivered more than once, so a failed attempt leaked its events: {joined:?}"
    );
}

/// A stream that ends without the provider's completion signal is a broken answer, not a
/// short one.
///
/// This is the shape a dropped connection usually takes: the body ends, and a body ending
/// looks exactly like a response being over. Nothing asked about the completion signal
/// before this -- `parser.done` was only ever used to leave the read loop early -- so a half
/// answer became *the* answer and the turn read as a success.
///
/// Retried here, because nothing had been drawn: this attempt produced only reasoning, which
/// goes to the status line and gets repainted anyway.
#[tokio::test]
async fn a_stream_that_stops_early_is_not_taken_for_an_answer() {
    let server = MockServer::start().await;
    // A reasoning fragment and then nothing: no `finish_reason`, no `[DONE]`.
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: sse(&[
                r#"data: {"choices":[{"delta":{"reasoning_content":"thinking about it"}}]}"#,
            ]),
        })
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answer_only(),
        })
        .with_priority(2)
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let mut seen = String::new();
    agent
        .run("hello", |event| {
            if let Event::Text(t) = event {
                seen.push_str(&t);
            }
        })
        .await
        .expect("a truncated stream that drew nothing must be retried, not reported");

    assert!(seen.contains("All done."), "the retry never produced an answer: {seen:?}");
}

/// Once the answer has started arriving, a failure is reported rather than retried.
///
/// This is the price of streaming, and it is paid here rather than by buffering the whole
/// answer: a second attempt would be written *after* the first, because nothing in the chain
/// can take text back -- a terminal has scrolled the line, a pipe has emitted it, the browser
/// has already rendered it. What the reader gets instead is what arrived, plus an error that
/// says what happened.
#[tokio::test]
async fn a_stream_that_dies_after_the_answer_started_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: sse(&[r#"data: {"choices":[{"delta":{"content":"half an ans"}}]}"#]),
        })
        .mount(&server)
        .await;

    let mut agent = agent_for(&server, std::env::temp_dir()).await;
    let error = agent.run("hello", |_| {}).await.unwrap_err();
    let message = format!("{error:#}");

    assert!(
        message.contains("before it was finished"),
        "it must say the stream was cut short: {message}"
    );
    assert!(
        message.contains("already begun to arrive"),
        "and that this is why it was not retried: {message}"
    );

    // One request, not four. The ladder stopping is the whole point: four would have printed
    // the answer four times.
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(
        requests.len(),
        1,
        "the retry ladder ran after text had been drawn, which doubles the answer"
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

/// A server that writes one delta, waits, then finishes.
///
/// A raw socket because no mock in the tree can hold a response open and dribble it out, and
/// holding it open is the whole of what is being tested: a client that buffers cannot report
/// the first delta until the wait is over.
async fn a_server_that_writes_slowly() -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a port");
    let port = listener.local_addr().expect("a port").port();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut head = [0u8; 4096];
        let _ = socket.read(&mut head).await;
        let _ = socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
                  data: {\"choices\":[{\"delta\":{\"content\":\"first \"}}]}\n\n",
            )
            .await;
        let _ = socket.flush().await;

        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let _ = socket
            .write_all(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"second\"}}]}\n\n\
                  data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                  data: [DONE]\n\n",
            )
            .await;
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

/// The answer arrives while the model is still writing it.
///
/// This is the feature, and it is only observable in time. The server sends one delta and
/// then sits on the connection for a second and a half, so a client that buffers the whole
/// response cannot report the first delta until the wait is over — which is exactly what
/// used to happen, and what made a local model feel slower than it is: a reasoning model
/// spends most of a turn producing text nobody could see.
#[tokio::test]
async fn the_answer_arrives_while_the_model_is_still_writing_it() {
    let (base_url, server) = a_server_that_writes_slowly().await;
    let config = test_config(&base_url);
    let provider = Provider::new(config.providers[0].clone()).expect("provider");
    let mut agent = Agent::new(&config, provider, false, std::env::temp_dir(), None);

    let started = std::time::Instant::now();
    let mut first_at: Option<std::time::Duration> = None;
    agent
        .run("hello", |event| {
            if matches!(event, Event::Text(_)) && first_at.is_none() {
                first_at = Some(started.elapsed());
            }
        })
        .await
        .expect("the turn must finish");

    let first_at = first_at.expect("no text reached the callback at all");
    assert!(
        first_at < std::time::Duration::from_millis(1000),
        "the first delta did not arrive for {first_at:?}, which is the whole response's worth \
         of waiting — the stream is still being collected rather than passed on"
    );
    let _ = server.await;
}
