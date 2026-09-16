//! `flint -p "..." --json`: the same run, written as one JSON object per line.
//!
//! The stream is a contract with a program rather than a terminal, so the assertions here
//! are on the bytes: every line parses on its own, the sequence says what happened in the
//! order it happened, and the answer comes out of the text fragments intact.
//!
//! These run the real binary against a stub OpenAI-compatible server, like the other
//! end-to-end tests: no API key, no network, and the whole path from argument parsing to
//! the last line of stdout is exercised for real.

use std::path::{Path, PathBuf};
use std::process::Command;

use flint::schema;
use serde_json::Value;
use wiremock::matchers::method;
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

/// A recorded SSE body: each line as its own event, which is what the provider expects.
fn sse(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\n\n");
    }
    out
}

/// A model answering with two text fragments and nothing else.
fn answers_in_two_fragments() -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"content":"hello "}}]}"#,
        r#"data: {"choices":[{"delta":{"content":"world"}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ])
}

/// A model asking for one `read`, which is the round after which it answers.
///
/// Built with `serde_json` rather than by hand: the arguments are a JSON string *inside* a
/// JSON string, and a Windows path is the case where writing that by hand goes wrong
/// silently -- the fixture produces a call whose arguments do not parse, and the test then
/// measures a tool that never ran.
fn asks_to_read(id: &str, path: &str) -> String {
    let arguments = serde_json::json!({ "path": path }).to_string();
    let frame = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": id,
                    "function": { "name": "read", "arguments": arguments }
                }]
            }
        }]
    });
    sse(&[
        &format!("data: {frame}"),
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// A scratch working directory for the run.
///
/// Deliberately a different name from the scratch `FLINT_HOME`: a home directory is emptied
/// when it is prepared, so a shared name means the fixture file written here is deleted by
/// the next line -- which reads as "the tool is broken" rather than "the test is".
fn cwd_for(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-json-cwd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("working directory");
    dir
}

/// A scratch `FLINT_HOME` whose configuration points at the stub server.
fn home_for(tag: &str, base_url: &str, cwd: &Path) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-json-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("home directory");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\
             \n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{base_url}\"\n\
             api_key = \"test\"\n\
             model = \"stub-model\"\n"
        ),
    )
    .expect("config file");
    // The working directory is written into the session's first event, so it has to be a
    // real one.
    std::fs::create_dir_all(cwd).expect("working directory");
    dir
}

/// A scratch home whose provider block is written by the caller.
///
/// The exit code a caller branches on depends on *why* the run ended, so these tests need homes
/// that differ in exactly one way -- no key, a step limit -- and a fixture that can only make one
/// kind of home would leave those cases untested or tested by a second copy of it.
fn home_configured(tag: &str, extra: &str, provider_block: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-json-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("home directory");
    std::fs::write(
        dir.join("config.toml"),
        format!("default_provider = \"stub\"\n{extra}\n[[providers]]\n{provider_block}\n"),
    )
    .expect("config file");
    dir
}

/// A stub answer that asks for a tool, for ever: the model never gets to finish.
fn always_asks_for_a_tool() -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"content":"looking. "}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// Run the real binary, parse the stream, and return (exit code, lines, stderr).
fn run_json(home: &Path, cwd: &Path, args: &[&str]) -> (i32, Vec<Value>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(args)
        .arg("--cwd")
        .arg(cwd)
        .env("FLINT_HOME", home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let lines = stdout
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("a line of the stream is not one JSON object: {line:?} ({e})")
            })
        })
        .collect();
    (
        out.status.code().unwrap_or(-1),
        lines,
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line["type"]
                .as_str()
                .unwrap_or_else(|| panic!("a line has no type: {line}"))
                .to_string()
        })
        .collect()
}

fn line_of<'a>(lines: &'a [Value], kind: &str) -> &'a Value {
    lines
        .iter()
        .find(|line| line["type"] == kind)
        .unwrap_or_else(|| panic!("no {kind} line in {:?}", kinds(lines)))
}

/// The whole shape of a simple answer: where it runs, what was asked, the text as it
/// arrived, the finished message, and the totals.
#[tokio::test]
async fn an_answer_arrives_as_a_stream_of_objects() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answers_in_two_fragments(),
        })
        .mount(&server)
        .await;

    let cwd = cwd_for("answer");
    let home = home_for("answer", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);

    assert_eq!(code, 0, "the run failed: {stderr}");
    assert_eq!(
        stderr, "",
        "a --json run wrote to stderr; stdout is supposed to be the whole channel"
    );
    assert_eq!(
        kinds(&lines),
        vec![
            "session.started",
            "turn.started",
            "message.delta",
            "message.delta",
            "message.completed",
            "turn.completed",
        ],
        "the stream is not the sequence of what happened"
    );

    assert_eq!(line_of(&lines, "turn.started")["prompt"], "say hello");
    assert_eq!(line_of(&lines, "message.completed")["text"], "hello world");
    assert_eq!(line_of(&lines, "session.started")["model"], "stub-model");
    assert_eq!(
        line_of(&lines, "session.started")["cwd"],
        cwd.to_string_lossy().as_ref()
    );

    // The stream says where the record is, and the record is there: a view of the turn is
    // not a replacement for it.
    let session = line_of(&lines, "session.started")["session"]
        .as_str()
        .expect("a session path")
        .to_string();
    assert!(session.ends_with(".jsonl"), "{session}");
    assert!(
        Path::new(&session).is_file(),
        "the session file named by the stream does not exist: {session}"
    );
}

/// A turn that runs a tool reports it by name, with the arguments and the result.
///
/// The name is the point: the provider announces a call by id, and a stream that only ever
/// said `call_1` would be useless to read.
#[tokio::test]
async fn a_tool_round_names_the_tool_it_ran() {
    let server = MockServer::start().await;
    let cwd = cwd_for("tool");
    let target = cwd.join("notes.txt");
    std::fs::write(&target, "the file the model asked for\n").expect("fixture file");

    // The tool round first, then the answer, exactly as a provider would serve them.
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: asks_to_read("call_1", &target.to_string_lossy()),
        })
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answers_in_two_fragments(),
        })
        .mount(&server)
        .await;

    let home = home_for("tool", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "read the notes", "--json"]);
    assert_eq!(code, 0, "the run failed: {stderr}");

    let started = line_of(&lines, "tool.started");
    assert_eq!(started["name"], "read");
    assert_eq!(started["id"], "call_1");

    let args = line_of(&lines, "tool.args");
    assert_eq!(args["name"], "read", "the arguments do not name the tool");
    assert!(
        args["arguments"]
            .as_str()
            .unwrap_or_default()
            .contains("notes.txt"),
        "the arguments are not the ones the model sent: {args}"
    );

    let completed = line_of(&lines, "tool.completed");
    assert_eq!(completed["name"], "read");
    assert_eq!(completed["ok"], true, "the tool failed: {completed}");
    assert!(
        completed["output"]
            .as_str()
            .unwrap_or_default()
            .contains("the file the model asked for"),
        "the result is not the tool's output: {completed}"
    );

    // The stream still ends with the answer of the round that followed the tool.
    assert_eq!(line_of(&lines, "message.completed")["text"], "hello world");
}

/// A run that cannot even start is still described on the stream.
///
/// The failure this guards against is the quiet one: the reason goes to stderr, stdout is
/// empty, and a caller reading the stream sees an empty run and waits for a
/// `turn.completed` that is never coming. The stream has to say how the run ended even when
/// it ended before the first request.
#[tokio::test]
async fn a_run_that_cannot_start_ends_the_stream_with_an_error() {
    let server = MockServer::start().await;
    let cwd = cwd_for("no-key");
    let home = home_for("no-key", &server.uri(), &cwd);
    // A provider that is not local and has no key: the one thing that stops a run before it
    // starts, and the case a fresh machine actually hits.
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\
         \n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"https://api.example.invalid/v1\"\n\
         model = \"stub-model\"\n",
    )
    .expect("config file");

    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);

    // `EX_UNAVAILABLE`: a provider with no key is not a failed question, it is a provider that
    // cannot be asked at all, and the answer to a caller that retries is the same nothing.
    assert_eq!(code, exit_codes::UNAVAILABLE, "a run with no key must fail");
    assert_eq!(
        kinds(&lines),
        vec!["session.started", "turn.started", "error"],
        "the stream does not describe how the run ended"
    );
    assert!(
        line_of(&lines, "error")["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no API key"),
        "the error line does not say what was wrong: {lines:?}"
    );
    assert_eq!(
        stderr, "",
        "the reason was written to stderr instead of the stream"
    );
}

/// Without a prompt there is no run to describe, and saying so must not write half a
/// stream first: a caller reads stdout and would see an empty run as a successful one.
#[tokio::test]
async fn json_without_a_prompt_is_refused_without_writing_a_stream() {
    let server = MockServer::start().await;
    let cwd = cwd_for("refuse");
    let home = home_for("refuse", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["--json"]);

    assert_eq!(code, exit_codes::USAGE, "a missing prompt is the caller's command line");
    assert!(lines.is_empty(), "a refused run still wrote a stream: {lines:?}");
    assert!(
        stderr.contains("--json needs a prompt"),
        "the refusal does not say what is missing: {stderr}"
    );
}

/// A caller that has changed its mind can stop the run without killing the process.
///
/// A one-shot run has no keyboard, so `/stop` arrives on stdin -- the same word the REPL takes, for
/// the same reason: it is the interrupt that works when there is no key to press, which is exactly
/// the situation a caller is in. What has to be true: the run ends *promptly* rather than when the
/// model gets around to answering, it ends as a turn rather than as a crash, and the process is still
/// there to be read afterwards -- killing it would take the session file's last writes with it.
#[tokio::test]
async fn a_run_can_be_stopped_from_stdin() {
    struct Slow {
        body: String,
        delay: std::time::Duration,
    }

    impl Respond for Slow {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(self.delay)
                .set_body_string(self.body.clone())
        }
    }

    let server = MockServer::start().await;
    let cwd = cwd_for("stop");
    let home = home_for("stop", &server.uri(), &cwd);
    // Ten seconds of nothing, which is the wait `/stop` exists to cut short. The bound below is
    // eight, so a run that ends at ten ended because the model answered, not because it was stopped.
    Mock::given(method("POST"))
        .respond_with(Slow {
            body: answers_in_two_fragments(),
            delay: std::time::Duration::from_secs(10),
        })
        .mount(&server)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "say hello", "--json"])
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start flint");

    // Drained while the run is going: a caller reads a pipe as it arrives, and a process nobody is
    // reading would block on a full pipe buffer instead of on the model.
    let mut stdout = child.stdout.take().expect("a pipe");
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });

    let started = std::time::Instant::now();
    // Long enough that the turn is genuinely in flight -- the heartbeat is the proof of that, and the
    // assertion below checks it arrived.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("a pipe");
        stdin.write_all(b"/stop\n").expect("writing to flint");
        stdin.flush().expect("flushing");
    }

    let mut ended = None;
    while started.elapsed() < std::time::Duration::from_secs(8) {
        if let Some(status) = child.try_wait().expect("asking after flint") {
            ended = Some(status);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if ended.is_none() {
        let _ = child.kill();
    }
    // Reaped on every path, including the ones that already have the status: a killed child that is
    // never waited on is a zombie, and clippy is right to say so.
    let _ = child.wait();
    let text = reader.join().expect("the reader thread");
    let lines: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a line of the stream is JSON"))
        .collect();
    let stopped_after = started.elapsed();
    let _ = std::fs::remove_dir_all(&home);

    let status = ended.unwrap_or_else(|| {
        panic!(
            "/stop did not end the run: still going after {stopped_after:?}, and the stub answers at \
             ten seconds. Stream: {lines:?}"
        )
    });
    // Not 0, which is what this asserted when the stop was first built, and which was the bug: a
    // truncated answer and a finished one shared a success code, so a caller that branched on the
    // exit code -- which is what callers do -- acted on half an answer. The stream was honest
    // throughout; the code was not.
    assert_eq!(
        status.code(),
        Some(exit_codes::INTERRUPTED),
        "a stopped run must not exit 0: {:?}",
        kinds(&lines)
    );
    // And the stream says the same thing in the vocabulary a program reads.
    assert_eq!(
        line_of(&lines, "turn.completed")["outcome"],
        "stopped",
        "the end of a stopped turn does not say it was stopped: {lines:?}"
    );
    assert!(
        kinds(&lines).iter().all(|k| k != "message.delta"),
        "the stub's answer arrived after all, so the run was not cut short mid-request: {lines:?}"
    );
    assert!(
        stopped_after < std::time::Duration::from_secs(8),
        "the run outlived the stub's ten-second delay, so something other than /stop ended it"
    );
    assert!(
        kinds(&lines).iter().any(|k| k == "turn.completed"),
        "the stream does not say the turn ended: {lines:?}"
    );
    let said = lines
        .iter()
        .filter(|line| line["type"] == "warning")
        .map(|line| line["message"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        said.contains("stopped"),
        "nothing on the stream says the run was stopped, so a caller cannot tell this from an answer \
         that happened to be empty: {lines:?}"
    );
}

/// What the answer committed by a stop is worth: the words the caller already read.
///
/// `/stop` drops the turn's future, so the agent loop never reaches the code that turns a step's text
/// into a message. Without the commit, the half-answer exists on the caller's screen and nowhere
/// else -- nothing fails, and the next question about what it just read is answered as if it had
/// never been written. The REPL has committed it since the day that was reported; what a one-shot run
/// does with it was never tested, because the stub could not produce the shape this needs: text
/// arriving and *then* a stall. It can now, from a socket: the fixture below writes one delta and
/// then says nothing for ever, which is the cheapest way to have a drawn answer in hand at a stop.
#[tokio::test]
async fn a_stopped_run_keeps_what_it_had_drawn() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let base = format!("http://{}", listener.local_addr().expect("the address"));
    let _talker = std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        // The request has to be read before the answer is written: the client is still sending it, and
        // a server that replies to a half-written request is a fixture that would misbehave on a slow
        // machine rather than fail.
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match socket.read(&mut byte) {
                Ok(0) | Err(_) => return,
                Ok(_) => request.push(byte[0]),
            }
        }
        // No content-length and no chunking: the body ends when the connection does, and this one
        // never ends on its own -- which is the whole point.
        let _ = socket.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        );
        let _ = socket.write_all(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"half an \"}}]}\n\n",
        );
        let _ = socket.flush();
        // Held open, saying nothing. Long enough that the run below is stopped in the middle of it by
        // a test that takes two seconds, short enough not to hold the test process open.
        std::thread::sleep(std::time::Duration::from_secs(30));
    });

    let cwd = cwd_for("drawn");
    let home = home_for("drawn", &base, &cwd);
    let mut child = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "say hello", "--json"])
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start flint");
    let mut stdout = child.stdout.take().expect("a pipe");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });

    // Give the delta time to arrive and be reported before stopping: the stop has to catch a drawn
    // answer, not an empty one, or this test proves nothing.
    std::thread::sleep(std::time::Duration::from_millis(2000));
    {
        let mut stdin = child.stdin.take().expect("a pipe");
        stdin.write_all(b"/stop\n").expect("writing to flint");
        stdin.flush().expect("flushing");
    }
    let started = std::time::Instant::now();
    let mut ended = None;
    while started.elapsed() < std::time::Duration::from_secs(8) {
        if let Some(status) = child.try_wait().expect("asking after flint") {
            ended = Some(status);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if ended.is_none() {
        let _ = child.kill();
    }
    // Reaped on every path, including the ones that already have the status: a killed child that is
    // never waited on is a zombie, and clippy is right to say so.
    let _ = child.wait();
    let text = reader.join().expect("the reader thread");
    let events = session_events(&home);
    let _ = std::fs::remove_dir_all(&home);

    let status = ended.unwrap_or_else(|| {
        panic!("/stop did not end the run: the stub stalls for ever and the process was still going. Stream: {text}")
    });
    assert_eq!(
        status.code(),
        Some(exit_codes::INTERRUPTED),
        "the stopped run did not end cleanly. Stream: {text}"
    );
    assert!(
        text.contains("half an"),
        "the delta never reached the caller, so there was nothing drawn to keep: {text}"
    );
    let kept = events
        .iter()
        .filter(|event| event["type"] == "chat" && event["message"]["role"] == "assistant")
        .map(|event| event["message"]["content"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        kept.contains("half an"),
        "what the caller read is not in the session file, so asking about it later answers as if it \
         had never been written. Events: {events:?}"
    );
}

/// The exit code is the first thing a program looks at, so it has to classify the failure.
///
/// Every one of these used to be `1`, which tells a caller nothing: not whether to retry, not
/// whether the input was wrong, not whether the answer existed at all. The codes here are the
/// convention `sysexits.h` settled on, because "a CLI that always exits 0 (or always 1) hides this
/// signal, forcing agents to parse error text with regex" -- which is exactly what a caller of flint
/// had to do.
mod exit_codes {
    use super::*;

    pub(super) const USAGE: i32 = 2;
    pub(super) const DATAERR: i32 = 65;
    pub(super) const UNAVAILABLE: i32 = 69;
    pub(super) const INTERRUPTED: i32 = 130;

    /// A normal turn is a success, and says so twice: the code and the outcome.
    #[tokio::test]
    async fn a_finished_turn_is_a_success_with_a_complete_outcome() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-ok");
        Mock::given(method("POST"))
            .respond_with(SseFixture {
                body: answers_in_two_fragments(),
            })
            .mount(&server)
            .await;
        let home = home_for("code-ok", &server.uri(), &cwd);

        let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(code, 0, "a turn that answered failed: {stderr} {lines:?}");
        assert_eq!(
            line_of(&lines, "turn.completed")["outcome"],
            "complete",
            "the end of the turn does not say how it ended: {lines:?}"
        );
    }

    /// A turn that ran out of steps is not a failure and not a success: the answer is there and it
    /// is unfinished, and a caller that acts on it is acting on half of one.
    #[tokio::test]
    async fn a_turn_that_ran_out_of_steps_says_incomplete() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-steps");
        Mock::given(method("POST"))
            .respond_with(SseFixture {
                body: always_asks_for_a_tool(),
            })
            .mount(&server)
            .await;
        let home = home_configured(
            "code-steps",
            "max_steps = 1\n",
            &format!(
                "name = \"stub\"\nbase_url = \"{}\"\napi_key = \"test\"\nmodel = \"stub-model\"",
                server.uri()
            ),
        );
        std::fs::create_dir_all(&cwd).expect("working directory");

        let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "keep going", "--json"]);
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(
            line_of(&lines, "turn.completed")["outcome"],
            "incomplete",
            "a turn stopped by its own step limit ended as if it had finished: {stderr} {lines:?}"
        );
        assert_eq!(
            code, DATAERR,
            "an unfinished answer exited as if it were a usable one: {lines:?}"
        );
    }

    /// A provider that cannot be used is not a generic failure: nothing was asked of the model, and
    /// the fix is a key rather than a different question.
    #[tokio::test]
    async fn a_provider_with_no_key_is_unavailable() {
        let cwd = cwd_for("code-nokey");
        let home = home_configured(
            "code-nokey",
            "",
            "name = \"stub\"\nbase_url = \"https://example.invalid/v1\"\napi_key = \"\"\nmodel = \"stub-model\"",
        );
        std::fs::create_dir_all(&cwd).expect("working directory");

        let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "hello", "--json"]);
        let _ = std::fs::remove_dir_all(&home);
        assert!(
            kinds(&lines).iter().any(|k| k == "error"),
            "the unusable provider was not on the stream: {stderr} {lines:?}"
        );
        assert_eq!(
            code, UNAVAILABLE,
            "a missing key is not a generic failure: {lines:?}"
        );
    }

    /// A command line flint cannot act on is the caller's to fix, and must not look like a failed
    /// question -- the difference decides whether a program retries or reads its own arguments.
    #[tokio::test]
    async fn a_bad_command_line_is_a_usage_error() {
        let cwd = cwd_for("code-usage");
        std::fs::create_dir_all(&cwd).expect("working directory");
        let home = std::env::temp_dir().join(format!("flint-json-code-usage-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("home directory");

        let (code, _, _) = run_json(&home, &cwd, &["-p", "hello", "--json", "--not-a-flag"]);
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(code, USAGE, "an unknown flag is not a failure of the model");
    }

    /// An exhausted balance is not a rate limit, even when the provider uses the same status for
    /// both. This is the case that forced the classification to read the body: OpenAI-shaped
    /// endpoints report "you are out of money" as a `429`, and flint retried it four times with a
    /// 1+2+4+8-second backoff -- fifteen seconds to be told the same thing, twenty-five minutes
    /// across a hundred calls in a batch. The attempt count is the assertion that matters.
    #[tokio::test]
    async fn an_exhausted_quota_is_not_retried() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-quota");
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string(
                r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let home = home_for("code-quota", &server.uri(), &cwd);
        std::fs::create_dir_all(&cwd).expect("working directory");

        let began = std::time::Instant::now();
        let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "hello", "--json"]);
        let took = began.elapsed();
        let _ = std::fs::remove_dir_all(&home);

        let error = line_of(&lines, "error");
        assert_eq!(
            error["code"], "insufficient_balance",
            "the cause of the failure is not named: {stderr} {lines:?}"
        );
        assert_eq!(
            error["retryable"], false,
            "a caller is told to retry something that cannot succeed: {lines:?}"
        );
        assert_eq!(
            code, UNAVAILABLE,
            "an empty account is not a generic failure: {lines:?}"
        );
        assert!(
            took < std::time::Duration::from_secs(5),
            "the quota failure was retried: it took {took:?}"
        );
    }

    /// DeepSeek says the same thing with a status of its own, and it has always been left alone --
    /// but a caller could only recognise it by matching the message text.
    #[tokio::test]
    async fn a_402_says_the_balance_is_gone() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-402");
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(402).set_body_string(
                r#"{"error":{"message":"Insufficient Balance","type":"unknown_error","code":"invalid_request_error"}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let home = home_for("code-402", &server.uri(), &cwd);
        std::fs::create_dir_all(&cwd).expect("working directory");

        let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "hello", "--json"]);
        let _ = std::fs::remove_dir_all(&home);

        let error = line_of(&lines, "error");
        assert_eq!(
            error["code"], "insufficient_balance",
            "DeepSeek's own status is not classified: {stderr} {lines:?}"
        );
        assert_eq!(error["retryable"], false, "{lines:?}");
        assert_eq!(code, UNAVAILABLE, "{lines:?}");
    }

    /// A schema flint cannot check is a command-line mistake, not an answer that failed: nothing was
    /// asked, and changing the schema fixes it.
    #[test]
    fn an_unusable_schema_is_a_usage_error() {
        let cwd = cwd_for("code-schema");
        std::fs::create_dir_all(&cwd).expect("working directory");
        let home = std::env::temp_dir().join(format!("flint-json-code-schema-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("home directory");

        let (code, lines, stderr) = run_json(
            &home,
            &cwd,
            &["-p", "hello", "--json", "--schema", r#"{"type":"object","pattern":1}"#],
        );
        let _ = std::fs::remove_dir_all(&home);
        // The refusal reaches stderr, not stdout: the schema is resolved before the stream is opened,
        // so a `--json` caller sees an empty stdout and a non-zero code. Recorded rather than fixed
        // here -- `ROADMAP.md` §10 B7 is the item, and it belongs with the rest of "a run that cannot
        // start says nothing on the stream" -- but the code is what this test is about.
        assert!(
            stderr.contains("pattern") || stderr.contains("schema"),
            "the refusal does not say what is wrong: {stderr}"
        );
        assert!(
            lines.is_empty(),
            "unexpected output on a run that never started: {lines:?}"
        );
        assert_eq!(code, USAGE, "a schema this build cannot check is the caller's input");
    }
}

/// A run that is working but not talking has to say so, or a caller cannot tell it from a dead one.
///
/// This is the one gap the streaming interface had. `--json` flushes every line as it happens, so
/// a caller sees the answer being written -- but between `tool.started` and `tool.completed` there is
/// nothing at all, and a slow tool call, a slow model and a crashed process look identical from a
/// pipe: silence. The turn below takes six seconds without saying a word, and the stream has to
/// carry a status while it does, before the answer rather than after it.
#[tokio::test]
async fn a_silent_turn_says_it_is_still_working() {
    /// Answers after a delay, which is what "a slow provider or a slow tool" looks like from here.
    struct Slow {
        body: String,
        delay: std::time::Duration,
    }

    impl Respond for Slow {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(self.delay)
                .set_body_string(self.body.clone())
        }
    }

    let server = MockServer::start().await;
    let cwd = cwd_for("heartbeat");
    let home = home_for("heartbeat", &server.uri(), &cwd);
    Mock::given(method("POST"))
        .respond_with(Slow {
            body: answers_in_two_fragments(),
            delay: std::time::Duration::from_millis(6000),
        })
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);
    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(6000),
        "the stub did not actually delay, so this proves nothing"
    );

    let beats: Vec<&Value> = lines.iter().filter(|l| l["type"] == "status").collect();
    assert!(
        !beats.is_empty(),
        "six seconds of silence and not one status line: {lines:?}"
    );
    assert!(
        beats[0]["elapsed_secs"].as_u64().unwrap_or(0) >= 5,
        "the first status does not say how long the wait has been: {}",
        beats[0]
    );
    // Before the answer, not after it: a beat that arrives with the answer tells a caller nothing
    // about the silence it was meant to fill.
    let beat_at = lines.iter().position(|l| l["type"] == "status").expect("a status");
    let done_at = lines
        .iter()
        .position(|l| l["type"] == "message.completed")
        .expect("an answer");
    assert!(beat_at < done_at, "the status came after the answer: {lines:?}");
}

///
/// This is the rest of the machine interface. `--json` already made a run readable line by line;
/// a caller that has to *act* on the answer wants the answer as data, and the only thing the
/// OpenAI-compatible surface agrees on is `response_format: {"type": "json_object"}` -- which
/// promises the reply parses, not that it has the fields that were asked for. Measured against
/// DeepSeek: `json_schema` is rejected outright, so the shape goes into the prompt and flint
/// checks the answer itself, asking again with the specific problems when it does not match.
fn sse_text(content: &str) -> String {
    let frame = serde_json::json!({ "choices": [{ "delta": { "content": content } }] });
    sse(&[
        &format!("data: {frame}"),
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ])
}

fn schema_run() -> String {
    serde_json::json!({
        "type": "object",
        "properties": { "trading_day": { "type": "string" } },
        "required": ["trading_day"],
        "additionalProperties": false,
    })
    .to_string()
}

/// Every session file under a home, one level of project directory included.
fn session_files(home: &Path) -> Vec<PathBuf> {
    let root = home.join("sessions");
    let mut found: Vec<PathBuf> = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The events of the one session a run wrote.
fn session_events(home: &Path) -> Vec<Value> {
    let files = session_files(home);
    assert_eq!(files.len(), 1, "expected one session, found {files:?}");
    std::fs::read_to_string(&files[0])
        .expect("session file")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a session line is JSON"))
        .collect()
}

/// A stub that replays one answer per request, in order, and remembers what it was sent.
///
/// Recording the request is the point: `response_format` and the schema section of the prompt are
/// invisible in the stream, so the only honest way to assert they were sent is to read the body the
/// server received. The last replay is repeated, which is how "the model never gets it right" is
/// written.
struct Scripted {
    answers: Vec<String>,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Respond for Scripted {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let mut seen = self.seen.lock().expect("the recording is not poisoned");
        let index = seen.len().min(self.answers.len().saturating_sub(1));
        seen.push(body);
        drop(seen);
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(self.answers[index].clone())
    }
}

fn recorded(seen: &std::sync::Arc<std::sync::Mutex<Vec<Value>>>) -> Vec<Value> {
    seen.lock().expect("the recording is not poisoned").clone()
}

/// A schema run answers with a `result` the caller can act on, and the request says what shape.
#[tokio::test]
async fn a_schema_run_answers_with_a_checked_result() {
    let server = MockServer::start().await;
    let cwd = cwd_for("schema-ok");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text(r#"{"trading_day": "2026-10-21"}"#)],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_for("schema-ok", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &[
            "-p",
            "when is the last trading day?",
            "--json",
            "--schema",
            &schema_run(),
        ],
    );
    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");

    let result = line_of(&lines, "result");
    assert_eq!(result["json"]["trading_day"], "2026-10-21");
    assert_eq!(result["attempts"], 1, "a first-try answer is one attempt");

    // The two halves of the promise, both on the wire: the server is asked for an object, and the
    // prompt carries the shape it has to be.
    let bodies = recorded(&seen);
    assert_eq!(bodies.len(), 1, "one turn for one good answer");
    assert_eq!(
        bodies[0]["response_format"]["type"], "json_object",
        "the request does not ask for JSON at all: {}",
        bodies[0]
    );
    let prompt = bodies[0]["messages"][0]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        prompt.contains("trading_day") && prompt.to_lowercase().contains("json"),
        "the system prompt does not carry the schema and the word json: {prompt}"
    );
}

/// An answer of the wrong shape is not accepted; it is quoted back with what was wrong.
#[tokio::test]
async fn an_answer_of_the_wrong_shape_is_asked_for_again() {
    let server = MockServer::start().await;
    let cwd = cwd_for("schema-repair");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![
                sse_text(r#"{"day": 3}"#),
                sse_text(r#"{"trading_day": "2026-10-21"}"#),
            ],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_for("schema-repair", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "when?", "--json", "--schema", &schema_run()],
    );
    assert_eq!(code, 0, "the repair did not rescue the run: {stderr}");

    let result = line_of(&lines, "result");
    assert_eq!(result["attempts"], 2, "the second answer is what was accepted");
    assert_eq!(
        kinds(&lines)
            .iter()
            .filter(|k| *k == "turn.started")
            .count(),
        2,
        "a repair is a turn and has to read as one: {lines:?}"
    );

    // The second request has to *contain* the mistake: `{"day": 3}` was missing the required field,
    // and a repair turn that did not say so would be asking the same question again. Read from the
    // decoded body rather than from the raw JSON of it, because the answer the model gave is a
    // string *inside* the request, and a substring search on the escaped form tests the escaping.
    let bodies = recorded(&seen);
    let repair = bodies[1]["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        repair.contains("trading_day"),
        "the repair prompt does not name the missing field: {repair}"
    );
    assert!(
        repair.contains(r#"{"day": 3}"#),
        "the repair prompt does not quote the answer back: {repair}"
    );
}

/// A model that never gets it right ends the stream without a `result` at all.
#[tokio::test]
async fn a_schema_that_never_matches_ends_the_stream_with_an_error() {
    let server = MockServer::start().await;
    let cwd = cwd_for("schema-no");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text(r#"{"day": 3}"#)],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_for("schema-no", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "when?", "--json", "--schema", &schema_run()],
    );

    assert_eq!(
        code,
        exit_codes::DATAERR,
        "an answer that never matched the schema is unusable, and says so in the code"
    );
    assert!(
        !kinds(&lines).iter().any(|k| k == "result"),
        "a `result` line was emitted for an answer the schema did not accept: {lines:?}"
    );
    let error = line_of(&lines, "error")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        error.contains("$.trading_day") || error.contains("trading_day"),
        "the error does not say which part of the shape failed: {error}"
    );
    assert!(
        error.contains("3 attempts"),
        "the error does not say how many times it was asked: {error}"
    );
    assert_eq!(
        recorded(&seen).len(),
        3,
        "three attempts is what the limit promises"
    );
    assert_eq!(stderr, "", "the reason went to stderr instead of the stream");
}

/// The shape follows the session file: resuming is enough, and the flag is not passed again.
#[tokio::test]
async fn a_resumed_session_is_held_to_the_schema_its_file_records() {
    let server = MockServer::start().await;
    let cwd = cwd_for("schema-resume");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text(r#"{"trading_day": "2026-10-21"}"#)],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_for("schema-resume", &server.uri(), &cwd);
    let (code, _, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "when?", "--json", "--schema", &schema_run()],
    );
    assert_eq!(code, 0, "the first run failed: {stderr}");

    let written = session_events(&home);
    let recorded_schema = written
        .iter()
        .find(|event| event["type"] == "schema")
        .expect("the run did not record the shape it was held to");
    assert_eq!(
        recorded_schema["schema"]["required"][0], "trading_day",
        "the file kept something other than the caller's schema: {recorded_schema}"
    );

    // No `--schema` this time: the file is the only place the shape can come from.
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "again?", "--json", "--continue"]);
    assert_eq!(code, 0, "the resumed run failed: {stderr}");
    assert_eq!(
        line_of(&lines, "result")["json"]["trading_day"],
        "2026-10-21",
        "the resumed run was not held to the shape its file records: {lines:?}"
    );
    let bodies = recorded(&seen);
    assert_eq!(bodies.len(), 2, "one request per run");
    assert_eq!(
        bodies[1]["response_format"]["type"], "json_object",
        "the resumed request dropped the JSON mode: {}",
        bodies[1]
    );
    assert!(
        bodies[1]["messages"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("trading_day"),
        "the resumed prompt dropped the schema: {}",
        bodies[1]["messages"][0]
    );
}

/// `--no-schema` is how a caller says "not this time", and the file records that it was said.
#[tokio::test]
async fn no_schema_lets_a_resumed_session_answer_in_prose_again() {
    let server = MockServer::start().await;
    let cwd = cwd_for("schema-none");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![
                sse_text(r#"{"trading_day": "2026-10-21"}"#),
                sse_text("the last trading day is 2026-10-21"),
            ],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_for("schema-none", &server.uri(), &cwd);
    let (code, _, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "when?", "--json", "--schema", &schema_run()],
    );
    assert_eq!(code, 0, "the first run failed: {stderr}");

    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "say it plainly", "--json", "--continue", "--no-schema"],
    );
    assert_eq!(code, 0, "the prose run failed: {stderr}");
    assert!(
        !kinds(&lines).iter().any(|k| k == "result"),
        "a prose run emitted a result line: {lines:?}"
    );
    assert_eq!(
        line_of(&lines, "message.completed")["text"],
        "the last trading day is 2026-10-21"
    );

    // Dropped on the wire as well as in the prompt: a server still asked for `json_object` would
    // make prose impossible, and the caller would never learn why.
    let bodies = recorded(&seen);
    assert!(
        bodies[1].get("response_format").is_none(),
        "the prose run still asked for JSON: {}",
        bodies[1]
    );
    assert!(
        !bodies[1]["messages"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("trading_day"),
        "the prose prompt still carries the schema: {}",
        bodies[1]["messages"][0]
    );

    // And the file says so, rather than leaving the next reader to infer it from silence.
    let written = session_events(&home);
    let last_schema = written
        .iter()
        .rfind(|event| event["type"] == "schema")
        .expect("the clearing was not recorded");
    assert!(
        last_schema.get("schema").is_none() || last_schema["schema"].is_null(),
        "the last schema line still names a shape: {last_schema}"
    );
}

/// A schema flint cannot check is refused before the run, not after an answer.
#[tokio::test]
async fn a_schema_this_build_cannot_check_is_refused_before_the_run() {
    let server = MockServer::start().await;
    let cwd = cwd_for("schema-refuse");
    let home = home_for("schema-refuse", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &[
            "-p",
            "when?",
            "--json",
            "--schema",
            r#"{"oneOf":[{"type":"string"}]}"#,
        ],
    );

    assert_ne!(code, 0, "a schema that cannot be checked must not run");
    assert!(
        lines.is_empty(),
        "a refused run still wrote a stream: {lines:?}"
    );
    assert!(
        stderr.contains("oneOf"),
        "the refusal does not name the keyword flint cannot check: {stderr}"
    );
}

/// The subset is checked as a unit here: the module's own tests cover the keyword walk.
#[test]
fn a_schema_refuses_what_it_cannot_check() {
    let refused = schema::Schema::parse(r#"{"type":"object","properties":{"a":{"format":"date"}}}"#);
    assert!(
        refused.is_err(),
        "`format` was accepted, so an answer could be certified against a rule flint does not check"
    );
}
