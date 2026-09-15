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

    assert_eq!(code, 1, "a run with no key must fail");
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

    assert_eq!(code, 1);
    assert!(lines.is_empty(), "a refused run still wrote a stream: {lines:?}");
    assert!(
        stderr.contains("--json needs a prompt"),
        "the refusal does not say what is missing: {stderr}"
    );
}

/// Structured output: the answer shape a caller asked for, checked before it is handed over.
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

    assert_eq!(code, 1, "an answer that never matched the schema must fail");
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
