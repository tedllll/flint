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

/// Without a prompt there is no run to describe, and saying so must not write half a
/// stream first: a caller reads stdout and would see an empty run as a successful one.
#[tokio::test]
async fn json_without_a_prompt_is_refused_without_writing_a_stream() {
    let server = MockServer::start().await;
    let cwd = cwd_for("refuse");
    let home = home_for("refuse", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["--json"]);

    assert_eq!(code, 1);
    assert!(lines.is_empty(), "a refused run still wrote a stream: {lines:?}");
    assert!(
        stderr.contains("--json needs a prompt"),
        "the refusal does not say what is missing: {stderr}"
    );
}
